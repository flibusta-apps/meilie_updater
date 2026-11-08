pub mod config;
pub mod models;
pub mod updater;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use deadpool_postgres::Pool;
use futures::future::BoxFuture;
use sentry::ClientOptions;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use subtle::ConstantTimeEq;
use tower_http::trace::{self, TraceLayer};
use tracing::Level;
use tracing_subscriber::{
    filter::LevelFilter, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer,
};

fn sentry_error_layer<S>() -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    // Preserve the legacy ERROR-only event filter; do not turn lower levels
    // into Sentry errors, breadcrumbs, or structured logs.
    sentry_tracing::ErrorLayer::new().with_filter(LevelFilter::ERROR)
}

/// Function performing one update run; injectable so tests never touch real services.
type Runner = Arc<
    dyn Fn(Pool, updater::MeiliClient) -> BoxFuture<'static, Result<updater::RunResult, String>>
        + Send
        + Sync,
>;

struct AppState {
    last_run: Mutex<Option<updater::RunResult>>,
    scheduling: Mutex<Scheduling>,
    pool: Pool,
    meili_client: updater::MeiliClient,
    runner: Runner,
}

#[derive(Default)]
struct Scheduling {
    running: bool,
    rerun_requested: bool,
}

fn default_runner() -> Runner {
    Arc::new(|pool, client| {
        Box::pin(async move {
            updater::update(pool, client)
                .await
                .map_err(|err| err.to_string())
        })
    })
}

/// Checks the `Authorization` header against the configured API key.
fn authorize(headers: &HeaderMap) -> Result<(), (StatusCode, &'static str)> {
    let api_key = match headers.get("Authorization") {
        Some(v) => v,
        None => return Err((StatusCode::UNAUTHORIZED, "No api-key!")),
    };

    let api_key = match api_key.to_str() {
        Ok(v) => v,
        Err(_) => return Err((StatusCode::UNAUTHORIZED, "Invalid api-key header")),
    };

    if !bool::from(config::CONFIG.api_key.as_bytes().ct_eq(api_key.as_bytes())) {
        return Err((StatusCode::UNAUTHORIZED, "Wrong api-key!"));
    }
    Ok(())
}

/// Releases the "update in progress" flag when dropped, regardless of how the
/// owning scope exits (normal return, error, or panic).
struct RunningGuard<'a>(&'a Mutex<Scheduling>);

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).running = false;
    }
}

async fn health() -> &'static str {
    "OK"
}

async fn update(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(resp) = authorize(&headers) {
        return resp;
    }

    {
        let mut scheduling = state.scheduling.lock().unwrap_or_else(|p| p.into_inner());
        if scheduling.running {
            scheduling.rerun_requested = true;
            return (StatusCode::ACCEPTED, "Update rerun queued");
        }
        scheduling.running = true;
    }

    tokio::spawn(async move {
        let mut running_guard = Some(RunningGuard(&state.scheduling));
        loop {
            let started = std::time::Instant::now();
            let started_at_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let result = (state.runner)(state.pool.clone(), state.meili_client.clone()).await;
            let run = result.unwrap_or_else(|err| updater::RunResult {
                started_at_unix,
                duration_ms: started.elapsed().as_millis() as u64,
                outcome: "failed".into(),
                error: Some(err),
                indices: Vec::new(),
            });
            let busy = run.outcome == "busy";
            log::info!("Update run completed: {:?}", run);
            *state.last_run.lock().unwrap_or_else(|p| p.into_inner()) = Some(run);
            // A different replica owns the lock: retain this accepted trigger
            // and retry, rather than silently dropping it. Never spin hot.
            if busy {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            let mut scheduling = state.scheduling.lock().unwrap_or_else(|p| p.into_inner());
            if scheduling.rerun_requested {
                scheduling.rerun_requested = false;
                continue;
            }
            // Disarm before releasing the mutex: a new request can then start
            // a worker without the old guard clearing its running flag.
            std::mem::forget(running_guard.take());
            scheduling.running = false;
            break;
        }
    });

    (StatusCode::ACCEPTED, "Update started")
}

async fn status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(resp) = authorize(&headers) {
        return resp.into_response();
    }

    let last_run = state
        .last_run
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    Json(serde_json::json!({
        "running": state.scheduling.lock().unwrap_or_else(|p| p.into_inner()).running,
        "last_run": last_run,
    }))
    .into_response()
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
    let options = ClientOptions::new()
        .dsn(&config::CONFIG.sentry_dsn)
        .maybe_release(sentry::release_name!());

    let _guard = sentry::init(options);

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(sentry_tracing::SpanLayer::new().with_filter(LevelFilter::INFO))
        .with(sentry_error_layer())
        .init();

    let pool = updater::get_postgres_pool()
        .await
        .unwrap_or_else(|err| panic!("Failed to create postgres pool: {:?}", err));
    let meili_client = updater::get_meili_client()
        .unwrap_or_else(|err| panic!("Failed to create meilisearch client: {:?}", err));

    let app_state = Arc::new(AppState {
        last_run: Mutex::new(None),
        scheduling: Mutex::new(Scheduling::default()),
        pool,
        meili_client,
        runner: default_runner(),
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

    #[test]
    fn sentry_captures_only_errors_without_breadcrumbs() {
        let events = sentry::test::with_captured_events(|| {
            let subscriber = tracing_subscriber::registry()
                .with(sentry_tracing::SpanLayer::new().with_filter(LevelFilter::INFO))
                .with(sentry_error_layer());
            tracing::subscriber::with_default(subscriber, || {
                let _span = tracing::info_span!("request").entered();
                tracing::info!("not a Sentry event");
                tracing::warn!("not a breadcrumb");
                tracing::error!("dependency migration regression");
            });
        });
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].level, sentry::Level::Error);
        assert_eq!(
            events[0].message.as_deref(),
            Some("dependency migration regression")
        );
        assert!(events[0].breadcrumbs.is_empty());
    }

    /// Populates all env vars `config::CONFIG` needs so it can be loaded in
    /// tests without touching real infrastructure. Safe/idempotent to call
    /// repeatedly and from multiple tests.
    fn set_test_env() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| unsafe {
            std::env::set_var("API_KEY", "test-api-key");
            std::env::set_var("SENTRY_DSN", "https://public@example.com/1");
            std::env::set_var("POSTGRES_DB_NAME", "test");
            std::env::set_var("POSTGRES_HOST", "localhost");
            std::env::set_var("POSTGRES_PORT", "5432");
            std::env::set_var("POSTGRES_USER", "test");
            std::env::set_var("POSTGRES_PASSWORD", "test");
            std::env::set_var("MEILI_HOST", "http://localhost:7700");
            std::env::set_var("MEILI_MASTER_KEY", "test");
        });
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
            scheduling: Mutex::new(Scheduling {
                running: update_running,
                rerun_requested: false,
            }),
            pool,
            meili_client,
            // Never touch real services from tests.
            runner: Arc::new(|_, _| {
                Box::pin(async {
                    Ok(updater::RunResult {
                        started_at_unix: 0,
                        duration_ms: 0,
                        outcome: "success".into(),
                        error: None,
                        indices: vec![],
                    })
                })
            }),
        })
    }

    async fn body_string(body: Body) -> String {
        let bytes = body.collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    async fn trigger(state: Arc<AppState>) {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", HeaderValue::from_static("test-api-key"));
        let response = update(State(state), headers).await.into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn busy_requests_coalesce_and_runner_errors_replace_last_success() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let mut state = build_test_app_state(false).await;
        let calls_runner = calls.clone();
        let entered_runner = entered.clone();
        let release_runner = release.clone();
        Arc::get_mut(&mut state).unwrap().runner = Arc::new(move |_, _| {
            let calls = calls_runner.clone();
            let entered = entered_runner.clone();
            let release = release_runner.clone();
            Box::pin(async move {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                entered.add_permits(1);
                release.acquire().await.unwrap().forget();
                if call == 0 {
                    Ok(updater::RunResult {
                        started_at_unix: 1,
                        duration_ms: 1,
                        outcome: "success".into(),
                        error: None,
                        indices: vec![],
                    })
                } else {
                    Err("simulated timeout/error".into())
                }
            })
        });
        trigger(state.clone()).await;
        entered.acquire().await.unwrap().forget();
        for _ in 0..3 {
            trigger(state.clone()).await;
        }
        release.add_permits(1);
        entered.acquire().await.unwrap().forget();
        assert_eq!(
            state.last_run.lock().unwrap().as_ref().unwrap().outcome,
            "success"
        );
        release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !state.scheduling.lock().unwrap().running {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let last = state.last_run.lock().unwrap();
        let last = last.as_ref().unwrap();
        assert_eq!(last.outcome, "failed");
        assert_eq!(last.error.as_deref(), Some("simulated timeout/error"));
        assert!(last.started_at_unix > 1);
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
    async fn update_while_already_running_queues_rerun() {
        let app_state = build_test_app_state(true).await;
        let app = build_router(app_state.clone());

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
        assert_eq!(
            body_string(response.into_body()).await,
            "Update rerun queued"
        );
        assert!(app_state.scheduling.lock().unwrap().rerun_requested);
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
    async fn status_without_auth_returns_401() {
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

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn status_with_no_run_yet_reports_not_running() {
        let app_state = build_test_app_state(false).await;
        let app = build_router(app_state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .header("Authorization", "test-api-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_str(&body_string(response.into_body()).await).unwrap();
        assert_eq!(body["running"], false);
        assert!(body["last_run"].is_null());
    }
}
