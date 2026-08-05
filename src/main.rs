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

fn build_router(app_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", axum::routing::get(health))
        .route("/update", post(update))
        .route("/status", axum::routing::get(status))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(trace::DefaultMakeSpan::new().level(Level::INFO))
                .on_response(trace::DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(app_state)
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

    let app = build_router(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

    log::info!("Start webserver...");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
    log::info!("Webserver shutdown...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{HeaderValue, Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Populates all env vars `config::CONFIG` needs so it can be loaded in
    /// tests without touching real infrastructure. Safe/idempotent to call
    /// repeatedly and from multiple tests.
    fn set_test_env() {
        unsafe {
            std::env::set_var("API_KEY", "test-api-key");
            std::env::set_var("SENTRY_DSN", "https://public@example.com/1");
            std::env::set_var("POSTGRES_DB_NAME", "test");
            std::env::set_var("POSTGRES_HOST", "localhost");
            std::env::set_var("POSTGRES_PORT", "5432");
            std::env::set_var("POSTGRES_USER", "test");
            std::env::set_var("POSTGRES_PASSWORD", "test");
            std::env::set_var("MEILI_HOST", "http://localhost:7700");
            std::env::set_var("MEILI_MASTER_KEY", "test");
        }
    }

    /// Builds an `AppState` backed by lazily-connecting Postgres/Meilisearch
    /// clients pointed at the dummy env values above; no real infra required
    /// since neither client connects eagerly.
    async fn build_test_app_state(update_running: bool) -> Arc<AppState> {
        set_test_env();

        let pool = updater::get_postgres_pool()
            .await
            .expect("failed to build test postgres pool");
        let meili_client =
            updater::get_meili_client().expect("failed to build test meilisearch client");

        Arc::new(AppState {
            last_run: Mutex::new(None),
            update_running: AtomicBool::new(update_running),
            pool,
            meili_client,
        })
    }

    async fn body_string(body: Body) -> String {
        let bytes = body.collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let app_state = build_test_app_state(false).await;
        let app = build_router(app_state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_string(response.into_body()).await, "OK");
    }

    #[tokio::test]
    async fn update_without_auth_header_returns_401() {
        let app_state = build_test_app_state(false).await;
        let app = build_router(app_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/update")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_string(response.into_body()).await, "No api-key!");
    }

    #[tokio::test]
    async fn update_with_invalid_header_bytes_returns_401() {
        let app_state = build_test_app_state(false).await;
        let app = build_router(app_state);

        let mut request = Request::builder()
            .method("POST")
            .uri("/update")
            .body(Body::empty())
            .unwrap();
        request.headers_mut().insert(
            "Authorization",
            HeaderValue::from_bytes(&[0xFF, 0xFE]).unwrap(),
        );

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            body_string(response.into_body()).await,
            "Invalid api-key header"
        );
    }

    #[tokio::test]
    async fn update_with_wrong_api_key_returns_401() {
        let app_state = build_test_app_state(false).await;
        let app = build_router(app_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/update")
                    .header("Authorization", "wrong-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_string(response.into_body()).await, "Wrong api-key!");
    }

    #[tokio::test]
    async fn update_while_already_running_returns_409() {
        let app_state = build_test_app_state(true).await;
        let app = build_router(app_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/update")
                    .header("Authorization", "test-api-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_string(response.into_body()).await,
            "Update already in progress"
        );
    }

    #[tokio::test]
    async fn update_with_correct_api_key_returns_202() {
        let app_state = build_test_app_state(false).await;
        let app = build_router(app_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/update")
                    .header("Authorization", "test-api-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(body_string(response.into_body()).await, "Update started");
    }

    #[tokio::test]
    async fn status_with_no_run_yet_returns_null() {
        let app_state = build_test_app_state(false).await;
        let app = build_router(app_state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_string(response.into_body()).await, "null");
    }
}
