use clap::clap_app;
use common::*;
use futures::prelude::*;
use hash_trie::HashTrie;
use reqwest::{header::USER_AGENT, Client};
use serde::Deserialize;
use serde_json::Value;
use std::io::{Read, Write};

async fn post(ids: impl Iterator<Item = &str>) -> Result<(), UserError> {
    const REDDIT_USER_AGENT: &str = concat!(
        "linux:xyz.tidder.op:v",
        env!("CARGO_PKG_VERSION"),
        " (by /u/CrackedP0t)"
    );

    let client = Client::new();

    let auth_resp = client
        .post("https://www.reddit.com/api/v1/access_token")
        .header(USER_AGENT, REDDIT_USER_AGENT)
        .basic_auth(
            &SECRETS.reddit.client_id,
            Some(&SECRETS.reddit.client_secret),
        )
        .form(&[
            ("grant_type", "password"),
            ("username", &SECRETS.reddit.username),
            ("password", &SECRETS.reddit.password),
        ])
        .send()
        .await?;

    let status = auth_resp.status();
    let json = auth_resp.json::<Value>().await?;

    if status.is_success() {
        let access_token = json["access_token"]
            .as_str()
            .ok_or_else(|| ue!("Access token not found"))?;

        let link = format!(
            "https://oauth.reddit.com/by_id/{}",
            ids.map(|id| format!("t3_{},", id)).collect::<String>()
        );

        let resp = client
            .get(&link)
            .header(USER_AGENT, REDDIT_USER_AGENT)
            .query(&[("raw_json", "1")])
            .bearer_auth(access_token)
            .send()
            .await?
            .error_for_status()?;

        for post in resp.json::<Value>().await?["data"]["children"]
            .as_array()
            .unwrap()
        {
            println!("{:#}", post["data"]);
        }

        Ok(())
    } else {
        println!("{:#}", json);

        Err(ue!(format!("Authentication HTTP error: {}", status)))
    }
}

async fn save(id: &str) -> Result<(), UserError> {
    let client = Client::new();

    let auth_resp = client
        .post("https://www.reddit.com/api/v1/access_token")
        .basic_auth(
            &SECRETS.reddit.client_id,
            Some(&SECRETS.reddit.client_secret),
        )
        .form(&[
            ("grant_type", "password"),
            ("username", &SECRETS.reddit.username),
            ("password", &SECRETS.reddit.password),
        ])
        .send()
        .await?;

    let status = auth_resp.status();
    let json = auth_resp.json::<Value>().await?;

    if status.is_success() {
        let access_token = json["access_token"]
            .as_str()
            .ok_or_else(|| ue!("Access token not found"))?;

        let link = format!("https://oauth.reddit.com/by_id/t3_{}", id);

        let resp = client
            .get(&link)
            .query(&[("raw_json", "1")])
            .header(USER_AGENT, "Tidder 0.0.1")
            .bearer_auth(access_token)
            .send()
            .await?
            .error_for_status()?;

        let post =
            Submission::deserialize(&resp.json::<Value>().await?["data"]["children"][0]["data"])?
                .finalize()?;

        let hash_saved = save_hash(&post.url, HashDest::Images).await?;

        if post.save(Ok(hash_saved.id)).await? {
            println!("already have");
        } else {
            println!("successfully saved");
        }
        Ok(())
    } else {
        println!("{:#}", json);

        Err(ue!(format!("Authentication HTTP error: {}", status)))
    }
}

async fn hash(links: &[&str]) -> Result<(), UserError> {
    futures::stream::iter(links.iter())
        .fold(None, move |last, arg| async move {
            let HashGotten { hash, end_link, .. } = match get_hash(&arg).await {
                Ok(res) => res,
                Err(e) => {
                    warn!("{} failed: {:?}", arg, e);
                    return last;
                }
            };

            let mut out = format!("{}: {}", end_link, hash);
            if let Some(last) = last {
                out = format!("{} ({})", out, distance(hash, last));
            }
            println!("{}", out);

            Some(hash)
        })
        .await;

    Ok(())
}

async fn search(link: &str, distance: Option<i64>) -> Result<(), UserError> {
    const DEFAULT_DISTANCE: i64 = 2;

    let distance = distance.unwrap_or(DEFAULT_DISTANCE);

    let hash = get_hash(link).await?.hash;

    let found = PG_POOL
        .get()
        .await?
        .query(
            "SELECT hash <-> $1 as distance, images.link, permalink, \
             score, author, created_utc, subreddit, title \
             FROM posts INNER JOIN images \
             ON hash <@ ($1, $2) \
             AND image_id = images.id \
             ORDER BY distance ASC, created_utc ASC",
            &[&hash, &distance],
        )
        .await?;

    for row in found {
        println!(
            "{} | {} | {} | {} | {} | /r/{} | {} | {}",
            row.get::<_, i64>("distance"),
            row.get::<_, chrono::NaiveDateTime>("created_utc"),
            row.get::<_, i64>("score"),
            row.get::<_, &str>("link"),
            row.get::<_, &str>("permalink"),
            row.get::<_, &str>("subreddit"),
            row.get::<_, &str>("author"),
            row.get::<_, &str>("title")
        );
    }

    Ok(())
}

async fn rank() -> Result<(), UserError> {
    let rows = PG_POOL
        .get()
        .await?
        .query(
            "SELECT COUNT(*) AS num,
             (SELECT link FROM images AS images2 WHERE images.hash <@ (images2.hash, 0) LIMIT 1) AS link
             FROM images GROUP BY hash ORDER BY num DESC LIMIT 100", &[]).await?;

    let commons = CommonImages {
        as_of: chrono::offset::Utc::now(),
        common_images: rows
            .iter()
            .map(|row| CommonImage {
                num: row.get::<_, i64>("num") as u64,
                link: row.get("link"),
            })
            .collect::<Vec<_>>(),
    };

    std::fs::File::create(std::env::var("HOME")? + "/stats/top100.ron")?
        .write_all(ron::ser::to_string_pretty(&commons, Default::default())?.as_bytes())?;

    Ok(())
}

async fn trie_build(path: &str, id_path: &str) -> Result<(), UserError> {
    let mut id_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(id_path)?;

    let mut old_id = String::new();
    id_file.read_to_string(&mut old_id)?;
    let old_id: i64 = if old_id.is_empty() {
        0
    } else {
        old_id.parse()?
    };

    let mut client = PG_POOL.get().await?;

    let trans = client.transaction().await?;

    println!("Startng to query...");

    let mut hashes = Box::pin(
        trans
            .query_raw(
                "SELECT DISTINCT (hash) hash FROM images WHERE id > $1",
                std::iter::once(&old_id as &dyn tokio_postgres::types::ToSql),
            )
            .await?,
    );

    println!("Recieved start of query!");

    let mut trie = HashTrie::<hash_trie::FileMap>::new(path.to_string());

    while let Some(row) = hashes.next().await {
        trie.insert(row?.get::<_, i64>("hash") as u64);
    }

    let last_id: i64 = trans
        .query_one("SELECT id FROM images ORDER BY id DESC LIMIT 1", &[])
        .await?
        .get("id");

    id_file.write_all(last_id.to_string().as_ref())?;

    trans.commit().await?;

    Ok(())
}

async fn trie_insert(path: &str, hashes: impl Iterator<Item = u64>) -> Result<(), UserError> {
    let mut trie = HashTrie::<hash_trie::FileMap>::new(path.to_string());

    for hash in hashes {
        let existed = trie.insert(hash);

        if existed {
            println!("Hash {:020} already exists", hash);
        } else {
            println!("Hash {:020} successfully inserted", hash);
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), UserError> {
    setup_logging!();

    let matches = clap_app!(op =>
        (@subcommand hash =>
         (@arg LINKS: +required ... "The links you wish to hash")
        )
        (@subcommand post =>
         (@arg ID: +required ... "Reddit's IDs for the posts")
        )
        (@subcommand rank => )
        (@subcommand save =>
         (@arg ID: +required "Reddit's ID for the post you wish to save")
        )
        (@subcommand search =>
         (@arg LINK: +required "The link to the image you wish to search for")
         (@arg distance: -d --distance +takes_value "The max distance you'll accept")
        )
        (@subcommand trie_build =>
         (@arg PATH: +required "The path to save the trie to")
         (@arg ID_PATH: +required "The path to save the last ID to")
        )
        (@subcommand trie_insert =>
         (@arg PATH: +required "The path of the trie file")
         (@arg HASHES: +required ... "The hashes you wish to save")
        )
    )
    .get_matches();

    let (op_name, op_matches) = matches.subcommand();
    let op_matches = op_matches.ok_or_else(|| ue!("No subcommand provided"))?;

    match op_name {
        "hash" => hash(&op_matches.values_of("LINKS").unwrap().collect::<Vec<_>>()).await,
        "post" => post(op_matches.values_of("ID").unwrap()).await,
        "rank" => rank().await,
        "save" => save(op_matches.value_of("ID").unwrap()).await,
        "search" => {
            search(
                op_matches.value_of("LINK").unwrap(),
                op_matches
                    .value_of("distance")
                    .map(|d| d.parse())
                    .transpose()?,
            )
            .await
        }
        "trie_build" => {
            trie_build(
                op_matches.value_of("PATH").unwrap(),
                op_matches.value_of("ID_PATH").unwrap(),
            )
            .await
        }
        "trie_insert" => {
            trie_insert(
                op_matches.value_of("PATH").unwrap(),
                op_matches
                    .values_of("HASHES")
                    .unwrap()
                    .try_fold(Vec::new(), |mut v, h| {
                        v.push(h.parse()?);
                        Ok::<_, UserError>(v)
                    })?
                    .iter()
                    .copied(),
            )
            .await
        }
        unknown => Err(ue!(format!("Unknown subcommand '{}'", unknown))),
    }
}
