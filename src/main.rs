#[macro_use]
extern crate lazy_static;

pub mod config;
pub mod models;
pub mod updater;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use deadpool_postgres::Pool;
use meilisearch_sdk::client::Client;
use sentry::{integrations::debug_images::DebugImagesIntegration, types::Dsn, ClientOptions};
use sentry_tracing::EventFilter;
use std::{
    net::SocketAddr,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use subtle::ConstantTimeEq;
use tower_http::trace::{self, TraceLayer};
use tracing::Level;
use tracing_subscriber::{filter, layer::SubscriberExt, util::SubscriberInitExt};

struct AppState {
    last_run: Mutex<Option<updater::RunResult>>,
    update_running: AtomicBool,
    pool: Pool,
    meili_client: Client,
}

/// Releases the "update in progress" flag when dropped, regardless of how the
/// owning scope exits (normal return, error, or panic).
struct RunningGuard<'a>(&'a AtomicBool);

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

async fn health() -> &'static str {
    "OK"
}

async fn update(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    let config_api_key = config::CONFIG.api_key.clone();

    let api_key = match headers.get("Authorization") {
        Some(v) => v,
        None => return (StatusCode::UNAUTHORIZED, "No api-key!"),
    };

    let api_key = match api_key.to_str() {
        Ok(v) => v,
        Err(_) => return (StatusCode::UNAUTHORIZED, "Invalid api-key header"),
    };

    if !bool::from(config_api_key.as_bytes().ct_eq(api_key.as_bytes())) {
        return (StatusCode::UNAUTHORIZED, "Wrong api-key!");
    }

    if state
        .update_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return (StatusCode::CONFLICT, "Update already in progress");
    }

    tokio::spawn(async move {
        let _running_guard = RunningGuard(&state.update_running);

        match updater::update(state.pool.clone(), state.meili_client.clone()).await {
            Ok(run_result) => {
                let any_failed = run_result.indices.iter().any(|i| !i.success);
                if any_failed {
                    log::error!("Update run completed with failures: {:?}", run_result);
                } else {
                    log::info!("Update run completed: {:?}", run_result);
                }
                *state
                    .last_run
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(run_result);
            }
            Err(err) => log::error!("Updater err: {:?}", err),
        };
    });

    (StatusCode::ACCEPTED, "Update started")
}

async fn status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let last_run = state
        .last_run
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    Json(last_run)
}

#[tokio::main]
async fn main() {
    let options = ClientOptions {
        dsn: Some(Dsn::from_str(&config::CONFIG.sentry_dsn).unwrap()),
        default_integrations: false,
        ..Default::default()
    }
    .add_integration(DebugImagesIntegration::new());

    let _guard = sentry::init(options);

    let sentry_layer = sentry_tracing::layer().event_filter(|md| match md.level() {
        &tracing::Level::ERROR => EventFilter::Event,
        _ => EventFilter::Ignore,
    });

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(filter::LevelFilter::INFO)
        .with(sentry_layer)
        .init();

    let pool = updater::get_postgres_pool()
        .await
        .unwrap_or_else(|err| panic!("Failed to create postgres pool: {:?}", err));
    let meili_client = updater::get_meili_client()
        .unwrap_or_else(|err| panic!("Failed to create meilisearch client: {:?}", err));

    let app_state = Arc::new(AppState {
        last_run: Mutex::new(None),
        update_running: AtomicBool::new(false),
        pool,
        meili_client,
    });

    let app = Router::new()
        .route("/health", axum::routing::get(health))
        .route("/update", post(update))
        .route("/status", axum::routing::get(status))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(trace::DefaultMakeSpan::new().level(Level::INFO))
                .on_response(trace::DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

    log::info!("Start webserver...");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
    log::info!("Webserver shutdown...")
}
