#[macro_use]
extern crate lazy_static;

pub mod config;
pub mod models;
pub mod updater;

use axum::{http::HeaderMap, routing::post, Router};
use sentry::{integrations::debug_images::DebugImagesIntegration, types::Dsn, ClientOptions};
use std::{net::SocketAddr, str::FromStr};

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
    env_logger::init();

    let options = ClientOptions {
        dsn: Some(Dsn::from_str(&config::CONFIG.sentry_dsn).unwrap()),
        default_integrations: false,
        ..Default::default()
    }
    .add_integration(DebugImagesIntegration::new());

    let _guard = sentry::init(options);

    let app = Router::new().route("/update", post(update));

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

    log::info!("Start webserver...");
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
    log::info!("Webserver shutdown...")
}
