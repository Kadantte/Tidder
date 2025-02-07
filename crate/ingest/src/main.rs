#![recursion_limit = "128"]

mod worker_limit;
use chrono::prelude::*;
use clap::Parser;
use common::*;
use dashmap::DashMap;
use future::poll_fn;
use futures::prelude::*;
use futures::task::Poll;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Deserializer;
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::convert::TryInto;
use std::error::Error as _;
use std::fs::{remove_file, File};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::iter::Iterator;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::time::{interval_at, Duration, Instant};
use tokio_postgres::types::ToSql;
use tracing_futures::Instrument;
use url::Url;

static POST_COUNT: AtomicU64 = AtomicU64::new(0);
static POSTS_PER_MINUTE: AtomicU64 = AtomicU64::new(0);

struct IngestInfo {
    month: u32,
    year: i32,
    already_have: Option<BTreeSet<i64>>,
}

async fn ingest_post(
    post: Submission,
    verbose: bool,
    blacklist: &DashMap<String, ()>,
    domains_in_flight: &DashMap<String, u32>,
) {
    if verbose {
        info!("Starting to ingest {}", post.url);
    }

    let post_url_res = (|| async {
        let post_url = post.choose_url()?;

        if get_host(post_url.as_str())
            .map(|host| blacklist.contains_key(&host))
            .unwrap_or(false)
        {
            return Err(ue_save!("blacklisted", "blacklisted"));
        }

        if CONFIG
            .banned
            .iter()
            .any(|banned| banned.matches(post_url.as_str()))
        {
            return Err(ue_save!("banned", "banned"));
        }

        Ok(post_url)
    })()
    .await;

    let save_res = match post_url_res {
        Ok(post_url) => {
            let host = post_url.host_str().unwrap();

            let custom_limit: Option<&Option<_>> = CONFIG.custom_limits.get(host);

            let limit = match custom_limit {
                None => Some(CONFIG.domains_in_flight_limit),
                Some(&Some(limit)) => Some(limit),
                Some(&None) => None,
            };

            poll_fn(|context| {
                let ready = limit
                    .map(|limit| {
                        domains_in_flight
                            .get(host)
                            .map(|domains_in_flight| *domains_in_flight < limit)
                            .unwrap_or(true)
                    })
                    .unwrap_or(true);

                if ready {
                    *(domains_in_flight.entry(host.to_owned()).or_insert(0)) += 1;

                    Poll::Ready(host.to_owned())
                } else {
                    context.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;

            if verbose {
                info!("Starting to save");
            }

            let res = save_hash(post_url.as_str(), HashDest::Images).await;

            *domains_in_flight.get_mut(host).unwrap() -= 1;

            res
        }
        Err(e) => Err(e),
    };

    let image_id = match save_res {
        Ok(hash_gotten) => {
            if verbose {
                info!("successfully hashed");
            }

            Ok(hash_gotten.id)
        }
        Err(ue) => match ue.source {
            Source::Internal => {
                eprintln!(
                    "{}{}{}\n{:#?}\n{:#?}",
                    ue.file.unwrap_or(""),
                    ue.line
                        .map(|line| Cow::Owned(format!("#{}", line)))
                        .unwrap_or(Cow::Borrowed("")),
                    if ue.file.is_some() || ue.line.is_some() {
                        ": "
                    } else {
                        ""
                    },
                    ue.error,
                    post
                );
                std::process::exit(1)
            }
            _ => {
                let reqwest_save_error = match ue.error.downcast_ref::<reqwest::Error>() {
                    Some(e) => {
                        let hyper_error =
                            e.source().and_then(|he| he.downcast_ref::<hyper::Error>());

                        if e.is_timeout() || hyper_error.is_some() {
                            if let Ok(url) = Url::parse(&post.url) {
                                if let Some(host) = url.host_str() {
                                    if !CONFIG.no_blacklist.iter().any(|n| host.ends_with(n)) {
                                        blacklist.insert(host.to_string(), ());
                                    }
                                }
                            }
                        }

                        e.status()
                            .map(|status| format!("http_{}", status.as_str()).into())
                            .or_else(|| {
                                if e.is_timeout() {
                                    Some("timeout".into())
                                } else {
                                    None
                                }
                            })
                            .or_else(|| hyper_error.map(|_| "hyper".into()))
                    }
                    None => None,
                };

                let save_error = ue.save_error.or(reqwest_save_error);

                warn!(
                    "failed to save{}: {}",
                    save_error
                        .as_ref()
                        .map(|se| Cow::Owned(format!(" ({})", se)))
                        .unwrap_or_else(|| Cow::Borrowed("")),
                    ue.error
                );

                Err(save_error)
            }
        },
    };

    match post.save(image_id).await {
        Ok(_) => {
            POST_COUNT.fetch_add(1, Ordering::SeqCst);
            info!("successfully saved");
        }
        Err(e) => {
            error!("post \n{:#?} \nfailed to save:\n{:?}", post, e);
            std::process::exit(1);
        }
    }
}

async fn ingest_json<R: Read + 'static>(
    verbose: bool,
    IngestInfo {
        month,
        year,
        mut already_have,
    }: IngestInfo,
    json_stream: R,
) {
    let json_iter = Deserializer::from_reader(json_stream).into_iter::<Submission>();

    let mut ff_day = None;

    let json_iter = json_iter.filter_map(move |post| {
        let post = match post {
            Ok(post) => post,
            Err(e) => {
                if e.is_data() {
                    if verbose {
                        warn!("{:?}", e);
                    }
                    return None;
                } else {
                    panic!("{:?}", e)
                }
            }
        };

        let post = post.finalize().unwrap();

        if post.desirable()
            && match already_have {
                None => true,
                Some(ref mut set) => {
                    let day = post.created_utc.day();
                    if ff_day.map(|ff_day| day > ff_day).unwrap_or(true) {
                        info!("Fast forwarding through {}", post.created_utc.date());
                        ff_day = Some(day);
                    }
                    let had = set.remove(&post.id_int);
                    if set.is_empty() {
                        info!("Done fast forwarding!");
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
    });

    tokio::spawn(async move {
        let minute = Duration::from_secs(60);

        let mut previous_count = 0u64;
        let mut previous_time = Instant::now();
        let mut count_interval = interval_at(Instant::now() + minute, minute);

        loop {
            count_interval.tick().await;

            let current_count = POST_COUNT.load(Ordering::SeqCst);
            let current_time = Instant::now();
            let current_speed = (u128::from(current_count - previous_count)
                * (current_time - previous_time).as_nanos()
                / minute.as_nanos())
            .try_into()
            .map_err(map_ue!())
            .unwrap();

            POSTS_PER_MINUTE.store(current_speed, Ordering::SeqCst);

            let pretty_config = ron::ser::PrettyConfig::new();

            let mut state_file = tokio::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .create(true)
                .open(&CONFIG.state_file)
                .await
                .map_err(map_ue!())
                .unwrap();
            state_file.set_len(0).await.map_err(map_ue!()).unwrap();
            state_file
                .write_all(
                    ron::ser::to_string_pretty(
                        &IngestState {
                            as_of: Utc::now().naive_utc(),
                            month,
                            year,
                            posts_per_minute: current_speed,
                            limited: worker_limit::is_limited(),
                        },
                        pretty_config,
                    )
                    .map_err(map_ue!())
                    .unwrap()
                    .as_bytes(),
                )
                .await
                .map_err(map_ue!())
                .unwrap();
            previous_count = current_count;
            previous_time = current_time;
        }
    });

    let blacklist = Arc::new(DashMap::<String, ()>::new());
    let domains_in_flight = Arc::new(DashMap::<String, u32>::new());

    info!("Starting ingestion!");

    worker_limit::BufferUnordered::new(futures::stream::iter(json_iter.map(|post| {
        let blacklist = blacklist.clone();
        let domains_in_flight = domains_in_flight.clone();

        tokio::spawn(Box::pin(async move {
            let span = info_span!(
                "ingest_post",
                id = post.id.as_str(),
                date = post.created_utc.to_string().as_str(),
                url = post.url.as_str()
            );
            ingest_post(post, verbose, &blacklist, &domains_in_flight)
                .instrument(span)
                .await;
        }))
    })))
    .map(|t| t.unwrap())
    .collect::<()>()
    .await
}

#[derive(Parser)]
#[command(author, version, about, long_about = "none")]
struct Cli {
    #[arg(long, short = 'D')]
    no_delete: bool,
    #[arg(long, short)]
    verbose: bool,
    path: String,
}

#[tokio::main]
async fn main() -> Result<(), UserError> {
    static DATE_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(\d\d\d\d)-(\d\d)(?:-(\d\d))?").unwrap());

    tracing_subscriber::fmt::init();

    let args = Cli::parse();

    let verbose = args.verbose;
    let path = args.path;

    let (year, month, day): (i32, u32, Option<u32>) = DATE_RE
        .captures(&path)
        .ok_or_else(|| ue!(format!("couldn't find date in {}", path)))
        .and_then(|caps| {
            Ok((
                caps.get(1)
                    .ok_or_else(|| ue!(format!("couldn't find year in {}", path)))?
                    .as_str()
                    .parse()
                    .map_err(map_ue!())?,
                caps.get(2)
                    .ok_or_else(|| ue!(format!("couldn't find month in {}", path)))?
                    .as_str()
                    .parse()
                    .map_err(map_ue!())?,
                caps.get(3)
                    .map(|s| s.as_str().parse().map_err(map_ue!()))
                    .transpose()?,
            ))
        })?;

    let date = NaiveDate::from_ymd(year, month, day.unwrap_or(1)).and_hms(0, 0, 0);

    let next_date = if let Some(day) = day {
        const MONTH_LENGTHS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

        // >= for leap years
        if day >= MONTH_LENGTHS[month as usize - 1] {
            if month == 12 {
                NaiveDate::from_ymd(year + 1, 1, 1)
            } else {
                NaiveDate::from_ymd(year, month + 1, 1)
            }
        } else {
            NaiveDate::from_ymd(year, month, day + 1)
        }
    } else if month == 12 {
        NaiveDate::from_ymd(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd(year, month + 1, 1)
    }
    .and_hms(0, 0, 0);

    info!("Ingesting {}", path);

    let (input_file, arch_path): (File, _) =
        if path.starts_with("http://") || path.starts_with("https://") {
            let arch_path = std::env::var("HOME")?
                + "/archives/"
                + Url::parse(&path)?
                    .path_segments()
                    .ok_or_else(|| ue!("cannot-be-a-base-url"))?
                    .next_back()
                    .ok_or_else(|| ue!("no last path segment"))?;

            let arch_file = if Path::exists(Path::new(&arch_path)) {
                info!("Found existing archive file");

                File::options().read(true).open(&arch_path)?
            } else {
                info!("Downloading archive file");
                let mut arch_file = File::options()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(&arch_path)
                    .map_err(map_ue!("archive file couldn't be opened"))?;

                let no_timeout_client = reqwest::Client::builder().build()?;

                let mut resp = no_timeout_client
                    .get(&path)
                    .send()
                    .await?
                    .error_for_status()?;

                while let Some(chunk) = resp.chunk().await? {
                    arch_file.write_all(&chunk)?;
                }

                arch_file.seek(SeekFrom::Start(0))?;

                arch_file
            };

            (arch_file, Some(arch_path))
        } else {
            (File::open(&path)?, None)
        };

    info!("Processing posts we already have");

    let client = PG_POOL.get().await?;
    let already_have = client
        .query_raw(
            "SELECT reddit_id_int FROM posts \
             WHERE created_utc >= $1 and created_utc < $2",
            [&date as &dyn ToSql, &next_date as &dyn ToSql]
                .iter()
                .copied(),
        )
        .await?
        .try_fold(BTreeSet::new(), move |mut already_have, row| async move {
            already_have.insert(row.get(0));
            Ok(already_have)
        })
        .await?;

    drop(client);

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

    let input = BufReader::new(input_file);

    let ingest_info = IngestInfo {
        month,
        year,
        already_have,
    };

    if path.ends_with("bz2") {
        ingest_json(verbose, ingest_info, bzip2::bufread::BzDecoder::new(input)).await;
    } else if path.ends_with("xz") {
        ingest_json(verbose, ingest_info, xz2::bufread::XzDecoder::new(input)).await;
    } else if path.ends_with("zst") {
        let mut zstd_decoder = zstd::Decoder::new(input)?;
        zstd_decoder.set_parameter(zstd::stream::raw::DParameter::WindowLogMax(31))?;
        ingest_json(verbose, ingest_info, zstd_decoder).await;
    } else if path.ends_with("gz") {
        ingest_json(verbose, ingest_info, flate2::bufread::GzDecoder::new(input)).await;
    } else {
        ingest_json(verbose, ingest_info, input).await;
    };

    if !args.no_delete {
        if let Some(arch_path) = arch_path {
            remove_file(arch_path)?;
        }
    }

    info!("Done ingesting {}", &path);

    Ok(())
}
