use super::*;

use future::{err, ok, result};
use reqwest::r#async::Response;
use tokio::prelude::*;

macros::multi_either!(2);

lazy_static! {
    static ref REQW_CLIENT: reqwest::r#async::Client = reqwest::r#async::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();
}

pub fn is_link_imgur(link: &str) -> bool {
    lazy_static! {
        static ref IMGUR_LINK_RE: Regex = Regex::new(r"^https?://(?:^|\.)imgur.com[:/]").unwrap();
    }

    IMGUR_LINK_RE.is_match(link)
}

pub fn is_link_gfycat(link: &str) -> bool {
    lazy_static! {
        static ref GFYCAT_LINK_RE: Regex = Regex::new(r"^https?://(?:^|\.)gfycat.com[:/]").unwrap();
    }

    GFYCAT_LINK_RE.is_match(link)
}

lazy_static! {
    static ref WIKIPEDIA_FILE_RE: Regex =
        Regex::new(r"(?:^|\.)(?:wikipedia|wiktionary|wikiquote|wikibooks|wikisource|wikinews|wikiversity|wikispecies|mediawiki|wikidata|wikivoyage|wikimedia).org/wiki/((?:Image|File):[^#?]+)").unwrap();
}

pub fn is_wikipedia_file(link: &str) -> bool {
    WIKIPEDIA_FILE_RE.is_match(link)
}

pub fn is_link_special(link: &str) -> bool {
    is_link_imgur(link) || is_link_gfycat(link) || is_wikipedia_file(link)
}

// pub fn follow_link(url: &Url) -> Result<Option<String>, UserError> {
//     if let Some(link) = follow_wikipedia(url)? {
//         return Ok(Some(link));
//     }

//     if EXT_RE.is_match(url.as_str()) {
//         return Ok(None);
//     }

//     if url.path() == "/" {
//         return Ok(None);
//     }

//     if is_link_imgur(url.as_str()) {
//         follow_imgur(&url).map(Some)
//     } else if is_link_gfycat(url.as_str()) {
//         follow_gfycat(&url).map(Some)
//     } else {
//         Ok(None)
//     }
// }

// pub fn follow_gfycat(url: &Url) -> Result<String, UserError> {
//     lazy_static! {
//         static ref GFY_ID_SEL: Regex = Regex::new(r"^/([[:alpha:]]+)").unwrap();
//     }

//     #[derive(Deserialize)]
//     struct GfyItem {
//         #[serde(rename = "mobilePosterUrl")]
//         mobile_poster_url: String,
//     }

//     #[derive(Deserialize)]
//     struct Gfycats {
//         #[serde(rename = "gfyItem")]
//         gfy_item: GfyItem,
//     }

//     Ok(REQW_CLIENT
//         .get(&format!(
//             "https://api.gfycat.com/v1/gfycats/{}",
//             GFY_ID_SEL
//                 .captures(url.path())
//                 .and_then(|c| c.get(1))
//                 .map(|m| m.as_str())
//                 .ok_or_else(|| ue!("couldn't find Gfycat ID in link", Source::User))?
//         ))
//         .send()
//         .map_err(map_ue!("couldn't reach Gfycat API"))?
//         .error_for_status()
//         .map_err(error_for_status_ue)?
//         .json::<Gfycats>()
//         .map_err(map_ue!("invalid JSON from Gfycat API"))?
//         .gfy_item
//         .mobile_poster_url)
// }

// pub fn follow_imgur(url: &Url) -> Result<String, UserError> {
//     lazy_static! {
//         static ref IMGUR_SEL: Selector = Selector::parse("meta[property='og:image']").unwrap();
//         static ref IMGUR_GIFV_RE: Regex = Regex::new(r"([^.]+)\.(?:gifv|webm|mp4)$").unwrap();
//         static ref IMGUR_EMPTY_RE: Regex = Regex::new(r"^/\.[[:alnum:]]+\b").unwrap();
//         static ref IMGUR_EXT_RE: Regex =
//             Regex::new(r"[[:alnum:]]\.(?:jpg|png)[[:alnum:]]+").unwrap();
//     }

//     let path = url.path();
//     let link = url.as_str();
//     let path_start = url
//         .path_segments()
//         .and_then(|mut ps| ps.next())
//         .ok_or(ue!("base Imgur URL", Source::User))?;

//     if IMGUR_GIFV_RE.is_match(path) {
//         Ok(IMGUR_GIFV_RE
//             .replace(path, "https://i.imgur.com/$1.gif")
//             .to_string())
//     } else if IMGUR_EXT_RE.is_match(path) || path_start == "download" {
//         Ok(url.to_string())
//     } else {
//         let mut resp = REQW_CLIENT
//             .get(link)
//             .send()
//             .and_then(|resp| {
//                 if resp.status() == StatusCode::NOT_FOUND && path_start == "gallery" {
//                     REQW_CLIENT
//                         .get(&link.replace("/gallery/", "/a/"))
//                         .send()
//                         .and_then(Response::error_for_status)
//                 } else {
//                     resp.error_for_status()
//                 }
//             })
//             .map_err(error_for_status_ue)?;

//         let mut doc_string = String::new();

//         resp.read_to_string(&mut doc_string)
//             .map_err(map_ue!("invalid response", Source::External))?;

//         let doc = Html::parse_document(&doc_string);
//         let og_image = doc
//             .select(&IMGUR_SEL)
//             .next()
//             .and_then(|el| el.value().attr("content"))
//             .ok_or_else(|| ue!("couldn't extract image from Imgur album"))?;

//         let mut image_url =
//             Url::parse(og_image).map_err(map_ue!("invalid image URL from Imgur"))?;
//         image_url.set_query(None); // Maybe take advantage of Imgur's downscaling?
//         if IMGUR_EMPTY_RE.is_match(image_url.path()) {
//             return Err(ue!("empty Imgur album"));
//         }

//         Ok(image_url.into_string())
//     }
// }

// pub fn follow_wikipedia(url: &Url) -> Result<Option<String>, UserError> {
//     #[derive(Debug, Deserialize)]
//     struct ImageInfo {
//         mime: String,
//         thumburl: String,
//         url: String,
//     }
//     #[derive(Debug, Deserialize)]
//     struct Page {
//         imageinfo: Vec<ImageInfo>,
//     }
//     #[derive(Debug, Deserialize)]
//     struct Query {
//         pages: std::collections::HashMap<String, Page>,
//     }
//     #[derive(Debug, Deserialize)]
//     struct APIQuery {
//         query: Query,
//     }

//     let title = if let Some(title) = WIKIPEDIA_FILE_RE
//         .captures(url.as_str())
//         .and_then(|c| c.get(1))
//         .map(|m| m.as_str())
//     {
//         percent_decode(title.as_bytes())
//             .decode_utf8()
//             .map_err(map_ue!("couldn't decode title", Source::User))?
//     } else {
//         return Ok(None);
//     };

//     let api_url = Url::parse_with_params(
//         &format!(
//             "https://{}/w/api.php",
//             url.domain().ok_or(ue!("no domain in Wikipedia URL"))?
//         ),
//         &[
//             ("action", "query"),
//             ("format", "json"),
//             ("prop", "imageinfo"),
//             ("iiprop", "url|mime"),
//             ("iiurlwidth", "500"),
//             ("titles", &title),
//         ],
//     )
//     .map_err(map_ue!("couldn't create Wikipedia API URL", Source::User))?;

//     let api_query = REQW_CLIENT
//         .get(api_url)
//         .send()
//         .map_err(map_ue!("couldn't query Wikipedia API"))?
//         .json::<APIQuery>()
//         .map_err(map_ue!("Wikipedia API returned problematic JSON"))?;

//     let imageinfo = api_query
//         .query
//         .pages
//         .into_iter()
//         .next()
//         .ok_or(ue!("Wikipedia API returned no pages", Source::User))?
//         .1
//         .imageinfo
//         .into_iter()
//         .nth(0)
//         .ok_or(ue!("Wikipedia API returned no images", Source::User))?;

//     Ok(Some(if IMAGE_MIMES.contains(&imageinfo.mime.as_str()) {
//         imageinfo.url
//     } else {
//         imageinfo.thumburl
//     }))
// }

pub fn get_hash(link: String) -> impl Future<Item = (Hash, String, GetKind), Error = UserError> {
    if link.len() > 2000 {
        return MultiEither2::V2(err(ue!("URL too long", Source::User)));
    }

    let url = match Url::parse(&link).map_err(map_ue!("not a valid URL", Source::User)) {
        Ok(url) => url,
        Err(e) => return MultiEither2::V2(err(e)),
    };

    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return MultiEither2::V2(err(ue!("unsupported scheme in URL", Source::User)));
    }

    // let link = follow_link(&url)?
    //     .map(Cow::Owned)
    //     .unwrap_or_else(|| Cow::Borrowed(link));

    if let Some((hash, hash_dest, id)) = match get_existing(&link) {
        Ok(r) => r,
        Err(e) => return MultiEither2::V2(err(e)),
    } {
        return MultiEither2::V2(ok((hash, link, GetKind::Cache(hash_dest, id))));
    }

    MultiEither2::V1(
        REQW_CLIENT
            .get(&utf8_percent_encode(&link, QUERY_ENCODE_SET).collect::<String>())
            .header(header::ACCEPT, IMAGE_MIMES.join(","))
            .header(
                header::USER_AGENT,
                "Mozilla/5.0 (X11; Linux x86_64; rv:66.0) Gecko/20100101 Firefox/66.0",
            )
            .send()
            .map_err(map_ue!("couldn't connect to image host"))
            .and_then(|resp| resp.error_for_status().map_err(error_for_status_ue))
            .and_then(|resp| {
                let url = resp.url();
                if url
                    .host_str()
                    .map(|host| host == "i.imgur.com")
                    .unwrap_or(false)
                    && url.path() == "/removed.png"
                {
                    return err(ue!("removed from Imgur"));
                }

                if let Some(ct) = resp.headers().get(header::CONTENT_TYPE) {
                    let ct = match ct
                        .to_str()
                        .map_err(map_ue!("non-ASCII Content-Type header"))
                    {
                        Ok(ct) => ct,
                        Err(e) => return err(e),
                    };
                    if !IMAGE_MIMES.contains(&ct) {
                        return err(ue!(format!("unsupported Content-Type: {}", ct)));
                    }
                }

                ok((
                    resp.headers().to_owned(),
                    resp.into_body()
                        .concat2()
                        .map_err(map_ue!("couldn't download image", Source::External)),
                ))
            })
            .and_then(|(headers, fut)| (ok(headers), fut))
            .and_then(|(headers, image)| match hash_from_memory(image.as_ref()) {
                Ok(hash) => ok((hash, link, GetKind::Request(headers))),
                Err(e) => err(e),
            }),
    )
}

pub fn save_hash(
    link: String,
    hash_dest: HashDest,
) -> impl Future<Item = (Hash, HashDest, i64, bool), Error = UserError> {
    get_hash(link).and_then(move |(hash, link, get_kind)| {
        let inner_result = move || {
            let poss_move_row = |hash: Hash,
                                 found_hash_dest: HashDest,
                                 id: i64|
             -> Result<(Hash, HashDest, i64, bool), UserError> {
                if hash_dest == found_hash_dest || hash_dest == HashDest::ImageCache {
                    Ok((hash, hash_dest, id, true))
                } else {
                    let mut client = DB_POOL.get().map_err(map_ue!())?;
                    let mut trans = client.transaction().map_err(map_ue!())?;
                    let rows = trans
                        .query(
                            "INSERT INTO images \
                             (link, hash, no_store, no_cache, expires, etag, \
                             must_revalidate, retrieved_on) \
                             VALUES (SELECT link, hash, no_store, no_cache, expires, etag,\
                             must_revalidate, retrieved_on FROM image_cache WHERE id = $1) \
                             RETURNING id",
                            &[&id],
                        )
                        .map_err(map_ue!())?;
                    trans.commit().map_err(map_ue!())?;

                    let mut trans = client.transaction().map_err(map_ue!())?;
                    trans
                        .query("DELETE FROM image_cache WHERE id = $1", &[&id])
                        .map_err(map_ue!())?;
                    trans.commit().map_err(map_ue!())?;

                    let id = rows
                        .get(0)
                        .and_then(|row| row.get("id"))
                        .unwrap_or_else(|| unreachable!());

                    Ok((hash, HashDest::Images, id, true))
                }
            };

            match get_kind {
                GetKind::Cache(hash_dest, id) => poss_move_row(hash, hash_dest, id),
                GetKind::Request(headers) => {
                    let now = chrono::offset::Utc::now().naive_utc();
                    let cc: Option<CacheControl> = headers
                        .get(header::CACHE_CONTROL)
                        .and_then(|hv| hv.to_str().ok())
                        .and_then(|s| cache_control::with_str(s).ok());
                    let cc = cc.as_ref();

                    let mut client = DB_POOL.get().map_err(map_ue!())?;
                    let mut trans = client.transaction().map_err(map_ue!())?;
                    let rows = trans
                        .query(
                            format!(
                                "INSERT INTO {} (link, hash, no_store, no_cache, expires, \
                                 etag, must_revalidate, retrieved_on) \
                                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                                 ON CONFLICT DO NOTHING \
                                 RETURNING id",
                                hash_dest.table_name()
                            )
                            .as_str(),
                            &[
                                &link,
                                &hash,
                                &cc.map(|cc| cc.no_store),
                                &cc.map(|cc| cc.no_cache),
                                &cc.and_then(|cc| cc.max_age)
                                    .map(|n| NaiveDateTime::from_timestamp(n as i64, 0))
                                    .or_else(|| {
                                        headers
                                            .get(header::EXPIRES)
                                            .and_then(|hv| hv.to_str().ok())
                                            .and_then(|s| DateTime::parse_from_rfc2822(s).ok())
                                            .map(|dt| dt.naive_utc())
                                    }),
                                &headers.get(header::ETAG).and_then(|hv| hv.to_str().ok()),
                                &cc.map(|cc| cc.must_revalidate),
                                &now,
                            ],
                        )
                        .map_err(map_ue!())?;
                    trans.commit().map_err(map_ue!())?;

                    match rows.get(0) {
                        Some(row) => Ok((
                            hash,
                            hash_dest,
                            row.try_get("id").map_err(map_ue!())?,
                            false,
                        )),
                        None => get_existing(&link)?
                            .map(|(hash, hash_dest, id)| poss_move_row(hash, hash_dest, id))
                            .ok_or_else(|| ue!("conflict but no existing match"))?,
                    }
                }
            }
        };
        result(inner_result())
    })
}
