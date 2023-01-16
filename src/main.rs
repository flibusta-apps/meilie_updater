#[macro_use]
extern crate lazy_static;

pub mod config;
pub mod updater;
pub mod models;

use axum::{http::HeaderMap, routing::post, Router};
use std::net::SocketAddr;

async fn update(headers: HeaderMap) -> &'static str {
    let config_api_key = config::CONFIG.api_key.clone();

    let api_key = match headers.get("Authorization") {
        Some(v) => v,
        None => return "No api-key!",
    };

    if config_api_key != api_key.to_str().unwrap() {
        return "Wrong api-key!";
    }

    tokio::spawn(async {
        match updater::update().await {
            Ok(_) => log::info!("Updated!"),
            Err(err) => log::info!("Updater err: {:?}", err),
        };
    });

    "Update started"
}

#[tokio::main]
async fn main() {
    let _guard = sentry::init(config::CONFIG.sentry_sdn.clone());
    env_logger::init();

    let app = Router::new().route("/update", post(update));

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

    log::info!("Start webserver...");
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
    log::info!("Webserver shutdown...")
}
