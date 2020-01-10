#[cfg(debug_assertions)]
use super::render::create_tera;
#[cfg(not(debug_assertions))]
use super::render::TERA;
use common::*;
use http::StatusCode;
use serde::Serialize;
use tera::Context;

#[derive(Serialize)]
struct Rankings {
    images: Vec<CommonImage>,
}

pub async fn get_response() -> Result<impl warp::Reply, UserError> {
    let rankings = Rankings {
        images: ron::de::from_reader(std::fs::File::open(
            std::env::var("HOME")? + "/stats/top100.ron",
        )?)?,
    };

    #[cfg(debug_assertions)]
    let tera = create_tera();

    #[cfg(not(debug_assertions))]
    let tera = TERA.force();

    let out = tera.render("rankings.html", &Context::from_serialize(&rankings)?)?;

    Ok(warp::reply::with_status(
        warp::reply::html(out),
        StatusCode::OK,
    ))
}
