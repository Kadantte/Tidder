#![type_length_limit = "5802293"]

use common::*;
use futures::TryFutureExt;
use once_cell::sync::Lazy;
use warp::filters::*;
use warp::{Filter, Rejection};

mod search;
use search::SearchQuery;
mod rankings;

mod render;

#[derive(Debug)]
struct UEReject(UserError);

impl warp::reject::Reject for UEReject {}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    Lazy::force(&render::TERA);

    let router = warp::path::end()
        .and(
            method::get()
                .and(query::query::<SearchQuery>().and_then(|query| async {
                    Ok::<_, Rejection>(search::get_response(query).await)
                }))
                .or(method::post()
                    .and(multipart::form())
                    .and_then(|form| async move {
                        Ok::<_, Rejection>(search::post_response(form).await)
                    })),
        )
        .or(warp::path::path("rankings").and_then(|| async {
            rankings::get_response()
                .map_err(|ue| {
                    println!("{:?}", ue);
                    warp::reject::custom(UEReject(ue))
                })
                .await
        }))
        .with(warp::log("site"));

    let ip = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1".to_string());

    let port = std::env::args()
        .nth(2)
        .map(|p| p.parse().unwrap())
        .unwrap_or(7878);

    println!("Serving on http://{}:{}", ip, port);

    warp::serve(router)
        .run((ip.parse::<std::net::IpAddr>().unwrap(), port))
        .await;
}
