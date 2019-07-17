#![recursion_limit = "128"]

use clap::{clap_app, crate_authors, crate_description, crate_version};
use common::*;
use failure::{format_err, Error};
use fallible_iterator::FallibleIterator;
use lazy_static::lazy_static;
use log::{error, info, warn};
use postgres::NoTls;
use r2d2_postgres::{r2d2, PostgresConnectionManager};
use regex::Regex;
use reqwest::{r#async::Client, StatusCode, Url};
use serde_json::from_value;
use serde_json::{Deserializer, Value};
use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use std::fs::{remove_file, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::iter::Iterator;
use std::path::Path;
use std::sync::{Arc, RwLock};
use tokio::prelude::*;

use future::{err, ok, result};

macros::multi_either!(2);

lazy_static! {
    static ref DB_POOL: r2d2::Pool<PostgresConnectionManager<NoTls>> = r2d2::Pool::new(
        PostgresConnectionManager::new(SECRETS.postgres.connect.parse().unwrap(), NoTls)
    )
    .unwrap();
}

struct Check<I> {
    iter: I,
}

impl<I> Check<I> {
    fn new(iter: I) -> Check<I> {
        Check { iter }
    }
}

impl<I, T, E> Iterator for Check<I>
where
    I: Iterator<Item = Result<T, E>>,
    E: std::fmt::Display,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match self.iter.next() {
            Some(res) => res.map(Some).map_err(le!()).unwrap_or(None),
            None => None,
        }
    }
}

fn ingest_json<R: Read + Send>(
    title: &str,
    mut already_have: Option<BTreeSet<i64>>,
    json_stream: R,
    verbose: bool,
) -> impl Future<Item = (), Error = ()> {
    let title = Arc::new(title.to_string());

    let json_iter = Deserializer::from_reader(json_stream).into_iter::<Value>();

    info!("Starting ingestion!");

    let check_json = Check::new(json_iter);

    // That's a lot of allocation...
    let to_submission = |mut post: Value| -> Result<Option<Submission>, Error> {
        let promo = post["promoted"].take();
        if !promo.is_null() && from_value(promo).map_err(Error::from)? {
            return Ok(None);
        }
        let id: String = from_value(post["id"].take()).map_err(Error::from)?;
        Ok(Some(Submission {
            id_int: i64::from_str_radix(&id, 36)
                .map_err(|e| format_err!("Couldn't parse number from ID '{}': {}", &id, e))?,
            id,
            author: from_value(post["author"].take()).map_err(Error::from)?,
            created_utc: match post["created_utc"].take() {
                Value::Number(n) => n
                    .as_i64()
                    .ok_or_else(|| format_err!("'created_utc' is not a valid i64"))?,
                Value::String(n) => n
                    .parse()
                    .map_err(|e| format_err!("'created_utc' can't be parsed as an i64: {}", e))?,
                _ => return Err(format_err!("'created_utc' is not a number or string")),
            },
            is_self: from_value(post["is_self"].take()).map_err(Error::from)?,
            over_18: from_value(post["over_18"].take()).map_err(Error::from)?,
            permalink: from_value(post["permalink"].take()).map_err(Error::from)?,
            score: from_value(post["score"].take()).map_err(Error::from)?,
            spoiler: from_value(post["spoiler"].take()).map_err(Error::from)?,
            subreddit: from_value(post["subreddit"].take()).map_err(Error::from)?,
            title: from_value(post["title"].take()).map_err(Error::from)?,
            url: from_value(post["url"].take()).map_err(Error::from)?,
        }))
    };

    // let thread_count = Arc::new(());
    let blacklist = Arc::new(RwLock::new(HashSet::<String>::new()));

    check_json
        .filter_map(|post| {
            let post = to_submission(post).map_err(le!()).ok()??;
            if !post.is_self
                && (EXT_RE.is_match(&post.url) || is_link_special(&post.url))
                && match already_have {
                    None => true,
                    Some(ref mut set) => {
                        let had = set.remove(&post.id_int);
                        if set.is_empty() {
                            already_have = None;
                        }
                        !had
                    }
                }
            {
                Some(post)
            } else {
                None
            }
        })
        .for_each(|mut post: Submission| {
            // let thread_count = thread_count.clone();
            let lazy_title = title.clone();
            let title = title.clone();
            let lazy_blacklist = blacklist.clone();
            let blacklist = blacklist.clone();
            tokio::spawn(
                future::lazy(move || {
                    let title = lazy_title;
                    let blacklist = lazy_blacklist;
                    post.url = post
                        .url
                        .replace("&amp;", "&")
                        .replace("&lt;", "<")
                        .replace("&gt;", ">");

                    let post_url = match Url::parse(&post.url) {
                        Ok(url) => url,
                        Err(e) => {
                            warn!("{}: {}: {} is invalid: {}", title, post.id, post.url, e);
                            return err(());
                        }
                    };

                    if post_url
                        .domain()
                        .map(|domain| blacklist.read().unwrap().contains(domain))
                        .unwrap_or(false)
                    {
                        if verbose {
                            warn!("{}: {}: {} is blacklisted", title, post.id, post.url);
                        }
                        return err(());
                    }

                    ok(post)
                })
                .and_then(move |post| {
                    let post_id = post.id.clone();
                    let post_url = post.url.clone();

                    let e_post_id = post.id.clone();
                    let e_post_url = post.url.clone();

                    let e_title = title.clone();

                    save_hash(post.url.clone(), HashDest::Images)
                        .and_then(move |(_hash, _hash_dest, image_id, exists)| {
                            (result(save_post(&DB_POOL, &post, image_id)), ok(exists))
                        })
                        .map(move |(post_exists, exists)| {
                            if !post_exists {
                                if verbose {
                                    if exists {
                                        info!(
                                            "{}: {}: {} already exists",
                                            title, post_id, post_url
                                        );
                                    } else {
                                        info!(
                                            "{}: {}: {} successfully hashed",
                                            title, post_id, post_url
                                        );
                                    }
                                }
                            } else {
                                warn!("{}: post ID {} already recorded", title, post_id);
                            }
                        })
                        .map_err(move |ue| {
                            let post_id = e_post_id;
                            let post_url = e_post_url;
                            match ue.source {
                                Source::Internal => {
                                    error!(
                                        "{}: {}: {}: {}{}{}{}",
                                        e_title,
                                        post_id,
                                        post_url,
                                        ue.file.unwrap_or(""),
                                        ue.line
                                            .map(|line| Cow::Owned(format!("#{}", line)))
                                            .unwrap_or(Cow::Borrowed("")),
                                        if ue.file.is_some() || ue.line.is_some() {
                                            ": "
                                        } else {
                                            ""
                                        },
                                        ue.error
                                    );
                                    std::process::exit(1);
                                }
                                _ => {
                                    if let Some(e) = ue.error.downcast_ref::<reqwest::Error>() {
                                        if let Some(StatusCode::NOT_FOUND) = e.status() {
                                            if !verbose {
                                                return;
                                            }
                                        } else if e.is_timeout()
                                            || e.get_ref()
                                                .and_then(|e| e.downcast_ref::<hyper::Error>())
                                                .map(hyper::Error::is_connect)
                                                .unwrap_or(false)
                                        {
                                            if is_link_special(&post_url) {
                                                error!(
                                                    "{}: {}: {}: Special link timed out",
                                                    e_title, post_id, post_url
                                                );
                                                std::process::exit(1);
                                            }
                                            if let Ok(url) = Url::parse(&post_url) {
                                                if let Some(domain) = url.domain() {
                                                    blacklist
                                                        .write()
                                                        .unwrap()
                                                        .insert(domain.to_string());
                                                }
                                            }
                                        }
                                    }
                                    warn!(
                                        "{}: {}: {} failed: {}",
                                        e_title, post_id, post_url, ue.error
                                    );
                                }
                            };
                        })
                })
                .then(move |_| {
                    // drop(thread_count);
                    ok(())
                }),
            );
        });

    // future::poll_fn(move || {
    // println!("{}", Arc::strong_count(&thread_count));
    // if Arc::strong_count(&thread_count) > 1 {
    //     Ok(Async::NotReady)
    // } else {
    //     println!("Ready!");
    //     Ok(Async::Ready(()))
    // }
    // })
    ok(())
}

fn main() {
    lazy_static::lazy_static! {
        static ref REQW_CLIENT: Client = Client::new();
        static ref MONTH_RE: Regex = Regex::new(r"(\d\d)\..+$").unwrap();
        static ref YEAR_RE: Regex = Regex::new(r"\d\d\d\d").unwrap();
    }

    setup_logging();
    let matches = clap_app!(
        ingest =>
            (version: crate_version!())
            (author: crate_authors!(","))
            (about: crate_description!())
            (@arg VERBOSE: -v --("verbose") "Verbose logging")
            (@arg PATHS: +required +multiple "The URLs or paths of the files to ingest")
    )
    .get_matches();

    let verbose = matches.is_present("VERBOSE");

    tokio::run(
        stream::iter_ok(matches.values_of_lossy("PATHS").unwrap())
            .and_then(move |path| {
                let month: i32 = MONTH_RE
                    .captures(&path)
                    .and_then(|caps| caps.get(1))
                    .ok_or_else(|| format_err!("couldn't find month in {}", path))
                    .and_then(|m| m.as_str().parse().map_err(Error::from))
                    .unwrap();

                let year: i32 = YEAR_RE
                    .find(&path)
                    .ok_or_else(|| format_err!("couldn't find year in {}", path))
                    .and_then(|m| m.as_str().parse().map_err(Error::from))
                    .unwrap();

                let month_f = f64::from(month);
                let year_f = f64::from(year);

                info!("Ingesting {}", &path);

                let (input_future, arch_path): (Box<Future<Item = File, Error = Error> + Send>, _) =
                    if path.starts_with("http://") || path.starts_with("https://") {
                        let arch_path = std::env::var("HOME").map_err(Error::from).unwrap()
                            + "/archives/"
                            + Url::parse(&path)
                                .map_err(Error::from)
                                .unwrap()
                                .path_segments()
                                .ok_or_else(|| format_err!("cannot-be-a-base-url"))
                                .unwrap()
                                .next_back()
                                .ok_or_else(|| format_err!("no last path segment"))
                                .unwrap();

                        let arch_file = if Path::exists(Path::new(&arch_path)) {
                            info!("Found existing archive file");

                            Box::new(future::result(
                                OpenOptions::new()
                                    .read(true)
                                    .open(&arch_path)
                                    .map_err(Error::from),
                            )) as _
                        } else {
                            info!("Downloading archive file");
                            let arch_file = OpenOptions::new()
                                .create_new(true)
                                .read(true)
                                .write(true)
                                .open(&arch_path)
                                .map_err(Error::from)
                                .unwrap();

                            Box::new(REQW_CLIENT.get(&path).send().map_err(Error::from).and_then(
                                move |resp| {
                                    resp.into_body()
                                        .map_err(Error::from)
                                        .fold(arch_file, |mut arch_file, chunk| {
                                            io::copy(&mut chunk.as_ref(), &mut arch_file)
                                                .map(move |_| arch_file)
                                                .map_err(Error::from)
                                        })
                                        .and_then(|mut arch_file| {
                                            arch_file
                                                .seek(SeekFrom::Start(0))
                                                .map_err(Error::from)?;

                                            Ok(arch_file)
                                        })
                                },
                            )) as _
                        };

                        (arch_file, Some(arch_path))
                    } else {
                        (
                            Box::new(future::result(File::open(&path).map_err(Error::from))) as _,
                            None,
                        )
                    };

                input_future.map_err(|e| panic!(e)).and_then(move |input| {
                    info!("Processing posts we already have");

                    let mut already_have = BTreeSet::new();

                    DB_POOL
                        .get()
                        .map_err(Error::from)
                        .unwrap()
                        .query_iter(
                            "SELECT reddit_id_int FROM posts \
                             WHERE EXTRACT(month FROM created_utc) = $1 \
                             AND EXTRACT(year FROM created_utc) = $2",
                            &[&month_f, &year_f],
                        )
                        .map_err(Error::from)
                        .unwrap()
                        .for_each(|row| {
                            already_have.insert(row.get(0));
                            Ok(())
                        })
                        .map_err(Error::from)
                        .unwrap();

                    let already_have_len = already_have.len();
                    info!(
                        "Already have {} post{}",
                        already_have_len,
                        if already_have_len == 1 { "" } else { "s" }
                    );

                    let already_have = if already_have_len > 0 {
                        Some(already_have)
                    } else {
                        None
                    };

                    let input = BufReader::new(input);

                    let title = format!("{:02}-{}", month, year);

                    let ingest_fut: Box<dyn future::Future<Item = (), Error = ()> + Send> =
                        if path.ends_with("bz2") {
                            Box::new(ingest_json(
                                &title,
                                already_have,
                                bzip2::bufread::BzDecoder::new(input),
                                verbose,
                            )) as _
                        } else if path.ends_with("xz") {
                            Box::new(ingest_json(
                                &title,
                                already_have,
                                xz2::bufread::XzDecoder::new(input),
                                verbose,
                            )) as _
                        } else if path.ends_with("zst") {
                            Box::new(ingest_json(
                                &title,
                                already_have,
                                zstd::stream::read::Decoder::new(input)
                                    .map_err(Error::from)
                                    .unwrap(),
                                verbose,
                            )) as _
                        } else {
                            Box::new(ingest_json(&title, already_have, input, verbose)) as _
                        };

                    ingest_fut.map(move |_| {
                        if let Some(arch_path) = arch_path {
                            remove_file(arch_path).map_err(Error::from).unwrap();
                        }

                        info!("Done ingesting {}", &path);
                    })
                })
            })
            .for_each(|_| ok(()))
            .map_err(|_| ()),
    );
}
