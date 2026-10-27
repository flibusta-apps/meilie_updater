use deadpool_postgres::{
    Config, CreatePoolError, ManagerConfig, Pool, PoolConfig, RecyclingMethod, Runtime, Timeouts,
};
use futures::{pin_mut, StreamExt};
use meilisearch_sdk::{
    client::*,
    errors::{Error as MeiliError, ErrorCode},
    request::{HttpClient, Method},
    reqwest::ReqwestClient,
    tasks::Task,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{error::Error as StdError, time::Duration};
use tokio_postgres::NoTls;

use crate::{
    config,
    models::{Author, Book, Genre, Sequence, UpdateModel},
};

#[derive(serde::Serialize, Clone, Debug)]
pub struct IndexRunResult {
    pub index: String,
    pub success: bool,
    pub document_count: Option<usize>,
    pub error: Option<String>,
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct RunResult {
    pub started_at_unix: u64,
    pub duration_ms: u64,
    pub indices: Vec<IndexRunResult>,
}

pub async fn get_postgres_pool() -> Result<Pool, CreatePoolError> {
    let mut config = Config::new();

    config.host = Some(config::CONFIG.postgres_host.clone());
    config.port = Some(config::CONFIG.postgres_port);
    config.dbname = Some(config::CONFIG.postgres_db_name.clone());
    config.user = Some(config::CONFIG.postgres_user.clone());
    config.password = Some(config::CONFIG.postgres_password.clone());
    config.connect_timeout = Some(std::time::Duration::from_secs(5));
    // Detect dead peers instead of hanging forever on a half-open connection.
    config.keepalives = Some(true);
    config.keepalives_idle = Some(Duration::from_secs(30));
    config.options = Some(format!(
        "-c statement_timeout={}",
        config::CONFIG.statement_timeout_ms
    ));
    config.manager = Some(ManagerConfig {
        recycling_method: RecyclingMethod::Verified,
    });
    config.pool = Some(PoolConfig {
        max_size: config::CONFIG.pool_max_size,
        timeouts: Timeouts {
            wait: Some(std::time::Duration::from_secs(
                config::CONFIG.pool_wait_timeout_secs,
            )),
            create: Some(std::time::Duration::from_secs(
                config::CONFIG.pool_create_timeout_secs,
            )),
            recycle: Some(std::time::Duration::from_secs(
                config::CONFIG.pool_recycle_timeout_secs,
            )),
        },
        ..Default::default()
    });

    match config.create_pool(Some(Runtime::Tokio1), NoTls) {
        Ok(pool) => Ok(pool),
        Err(err) => Err(err),
    }
}

/// `reqwest`-based Meilisearch HTTP client with a per-request timeout (the SDK
/// builds its own `reqwest::Client` without any timeouts).
#[derive(Clone, Debug)]
pub struct TimeoutHttpClient {
    inner: ReqwestClient,
    timeout: Duration,
}

#[async_trait::async_trait]
impl HttpClient for TimeoutHttpClient {
    async fn stream_request<
        Query: Serialize + Send + Sync,
        Body: futures_io::AsyncRead + Send + Sync + 'static,
        Output: DeserializeOwned + 'static,
    >(
        &self,
        url: &str,
        method: Method<Query, Body>,
        content_type: &str,
        expected_status_code: u16,
    ) -> Result<Output, MeiliError> {
        match tokio::time::timeout(
            self.timeout,
            self.inner
                .stream_request(url, method, content_type, expected_status_code),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(MeiliError::Timeout),
        }
    }
}

pub type MeiliClient = Client<TimeoutHttpClient>;

pub fn get_meili_client() -> Result<MeiliClient, MeiliError> {
    let api_key = config::CONFIG.meili_master_key.clone();
    let inner = ReqwestClient::new(Some(&api_key))?;
    Ok(Client::new_with_client(
        config::CONFIG.meili_host.clone(),
        Some(api_key),
        TimeoutHttpClient {
            inner,
            timeout: Duration::from_secs(config::CONFIG.meili_request_timeout_secs),
        },
    ))
}

struct TaskId(u32);

impl AsRef<u32> for TaskId {
    fn as_ref(&self) -> &u32 {
        &self.0
    }
}

type BoxError = Box<dyn StdError + Send>;

fn boxed<E: StdError + Send + 'static>(err: E) -> BoxError {
    Box::new(err)
}

const TASK_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Waits for a task to reach a final state and returns it (succeeded *or* failed).
async fn wait_task(
    task_info: meilisearch_sdk::task_info::TaskInfo,
    client: &MeiliClient,
    timeout: Duration,
) -> Result<Task, BoxError> {
    task_info
        .wait_for_completion(client, Some(TASK_POLL_INTERVAL), Some(timeout))
        .await
        .map_err(boxed)
}

fn ensure_task_succeeded(task: Task, context: &str) -> Result<(), BoxError> {
    match task {
        Task::Failed { content } => Err(boxed(std::io::Error::other(format!(
            "{context} task failed: {}",
            content.error
        )))),
        _ => Ok(()),
    }
}

/// True if the task failed because the index does not exist.
fn is_index_not_found_failure(task: &Task) -> bool {
    matches!(task, Task::Failed { content } if content.error.error_code == ErrorCode::IndexNotFound)
}

async fn wait_for_meili_task(
    task_info: meilisearch_sdk::task_info::TaskInfo,
    client: &MeiliClient,
    context: &str,
) -> Result<(), BoxError> {
    let task = wait_task(task_info, client, Duration::from_secs(120)).await?;
    ensure_task_succeeded(task, context)
}

/// Deletes `uid` if it exists, waiting for completion. A missing index is not an error:
/// Meilisearch accepts the deletion (202) and the *task* fails with `index_not_found`.
async fn delete_index_if_exists(meili_client: &MeiliClient, uid: &str) -> Result<(), BoxError> {
    match meili_client.delete_index(uid).await {
        Ok(task_info) => {
            let task = wait_task(task_info, meili_client, Duration::from_secs(120)).await?;
            if is_index_not_found_failure(&task) {
                return Ok(());
            }
            ensure_task_succeeded(task, "delete_index")
        }
        Err(MeiliError::Meilisearch(err)) if err.error_code == ErrorCode::IndexNotFound => Ok(()),
        Err(err) => Err(boxed(err)),
    }
}

/// Decides whether a freshly built index may replace the live one.
fn check_publish_allowed(
    new_docs: usize,
    live_docs: usize,
    max_shrink_ratio: f64,
    allow_empty: bool,
) -> Result<(), String> {
    if allow_empty {
        return Ok(());
    }
    if new_docs == 0 {
        return Err("new index is empty".to_string());
    }
    let min_allowed = (live_docs as f64) * (1.0 - max_shrink_ratio);
    if (new_docs as f64) < min_allowed {
        return Err(format!(
            "new index shrank too much: {new_docs} docs vs {live_docs} live (max shrink ratio {max_shrink_ratio})"
        ));
    }
    Ok(())
}

async fn update_model<T>(pool: Pool, meili_client: MeiliClient) -> Result<usize, BoxError>
where
    T: UpdateModel + Serialize + Send + Sync,
{
    let client = match pool.get().await {
        Ok(client) => client,
        Err(err) => return Err(Box::new(err)),
    };

    let live_uid = T::get_index();
    let staging_uid = format!("{live_uid}_new");

    // Clean up any leftover staging index from a previous crashed/failed run.
    delete_index_if_exists(&meili_client, &staging_uid).await?;

    // `swap_indexes` requires both index UIDs to already exist, so bootstrap an
    // empty live index on the very first run.
    match meili_client.get_index(&live_uid).await {
        Ok(_) => (),
        Err(MeiliError::Meilisearch(err)) if err.error_code == ErrorCode::IndexNotFound => {
            let task_info = match meili_client.create_index(&live_uid, Some("id")).await {
                Ok(task_info) => task_info,
                Err(err) => return Err(Box::new(err)),
            };
            wait_for_meili_task(task_info, &meili_client, "create_index (live)").await?;
        }
        Err(err) => return Err(Box::new(err)),
    };

    // Create the staging index and build the new dataset there.
    let task_info = match meili_client.create_index(&staging_uid, Some("id")).await {
        Ok(task_info) => task_info,
        Err(err) => return Err(Box::new(err)),
    };
    wait_for_meili_task(task_info, &meili_client, "create_index (staging)").await?;

    let staging_index = meili_client.index(&staging_uid);

    let task_info = match staging_index
        .set_searchable_attributes(T::get_searchable_attributes())
        .await
    {
        Ok(task_info) => task_info,
        Err(err) => return Err(Box::new(err)),
    };
    wait_for_meili_task(task_info, &meili_client, "set_searchable_attributes").await?;

    let task_info = match staging_index
        .set_filterable_attributes(T::get_filterable_attributes())
        .await
    {
        Ok(task_info) => task_info,
        Err(err) => return Err(Box::new(err)),
    };
    wait_for_meili_task(task_info, &meili_client, "set_filterable_attributes").await?;

    let task_info = match staging_index
        .set_ranking_rules(T::get_ranking_rules())
        .await
    {
        Ok(task_info) => task_info,
        Err(err) => return Err(Box::new(err)),
    };
    wait_for_meili_task(task_info, &meili_client, "set_ranking_rules").await?;

    let stream = match client
        .query_raw(&T::get_query(), std::iter::empty::<String>())
        .await
    {
        Ok(stream) => stream,
        Err(err) => return Err(Box::new(err)),
    };

    pin_mut!(stream);
    let mut chunks = stream.chunks(config::CONFIG.batch_size);

    let mut total_count: usize = 0;
    // Batches are only enqueued here; Meilisearch processes tasks in order, so
    // we wait for the last one and then verify every task result.
    let mut task_uids: Vec<u32> = Vec::new();
    let mut last_task_info = None;

    while let Some(chunk) = chunks.next().await {
        let mut items: Vec<T> = Vec::with_capacity(chunk.len());

        for result in chunk.into_iter() {
            let row = result.map_err(boxed)?;
            items.push(T::from_row(row).map_err(boxed)?);
        }

        total_count += items.len();

        let task_info = staging_index
            .add_or_update(&items, Some("id"))
            .await
            .map_err(boxed)?;
        task_uids.push(task_info.task_uid);
        last_task_info = Some(task_info);
    }

    if let Some(last) = last_task_info {
        let timeout = Duration::from_secs(config::CONFIG.update_timeout_secs);
        let task = wait_task(last, &meili_client, timeout).await?;
        ensure_task_succeeded(task, "add_or_update")?;

        for uid in task_uids {
            let task = meili_client.get_task(TaskId(uid)).await.map_err(boxed)?;
            ensure_task_succeeded(task, "add_or_update")?;
        }
    }

    // Safety net: never replace a healthy live index with an empty/truncated one.
    let new_docs = staging_index
        .get_stats()
        .await
        .map_err(boxed)?
        .number_of_documents;
    let live_docs = meili_client
        .index(&live_uid)
        .get_stats()
        .await
        .map_err(boxed)?
        .number_of_documents;
    if let Err(reason) = check_publish_allowed(
        new_docs,
        live_docs,
        config::CONFIG.max_shrink_ratio,
        config::CONFIG.allow_empty_index,
    ) {
        if let Err(err) = delete_index_if_exists(&meili_client, &staging_uid).await {
            log::error!("Failed to remove rejected staging index {staging_uid}: {err}");
        }
        return Err(boxed(std::io::Error::other(format!(
            "refusing to publish index {live_uid}: {reason}"
        ))));
    }

    // Atomically publish: swap live <-> staging so the live UID now serves the
    // freshly built dataset.
    let swap = SwapIndexes {
        indexes: (live_uid.clone(), staging_uid.clone()),
    };
    let task_info = meili_client.swap_indexes([&swap]).await.map_err(boxed)?;
    wait_for_meili_task(task_info, &meili_client, "swap_indexes").await?;

    // Best-effort cleanup: `staging_uid` now holds the old live data. Failing to
    // delete it does not affect correctness of the publish that already happened.
    match meili_client.delete_index(&staging_uid).await {
        Ok(task_info) => {
            if let Err(err) =
                wait_for_meili_task(task_info, &meili_client, "delete_index (post-swap)").await
            {
                log::error!(
                    "Failed to clean up staging index after swap: index={} err={}",
                    staging_uid,
                    err
                );
            }
        }
        Err(err) => {
            log::error!(
                "Failed to enqueue cleanup of staging index after swap: index={} err={}",
                staging_uid,
                err
            );
        }
    }

    Ok(total_count)
}

/// Aborts the wrapped tasks when dropped (e.g. when the run is cancelled by a timeout).
struct AbortOnDrop(Vec<tokio::task::AbortHandle>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        for handle in &self.0 {
            handle.abort();
        }
    }
}

pub async fn update(
    pool: Pool,
    meili_client: MeiliClient,
) -> Result<RunResult, Box<dyn std::error::Error>> {
    log::info!("Start update...");

    let started = std::time::Instant::now();
    let started_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let pool_clone = pool.clone();
    let meili_client_clone = meili_client.clone();
    let update_books_process =
        tokio::spawn(async move { update_model::<Book>(pool_clone, meili_client_clone).await });

    let pool_clone = pool.clone();
    let meili_client_clone = meili_client.clone();
    let update_authors_process =
        tokio::spawn(async move { update_model::<Author>(pool_clone, meili_client_clone).await });

    let pool_clone = pool.clone();
    let meili_client_clone = meili_client.clone();
    let update_sequences_process =
        tokio::spawn(async move { update_model::<Sequence>(pool_clone, meili_client_clone).await });

    let pool_clone = pool.clone();
    let meili_client_clone = meili_client.clone();
    let update_genres_process =
        tokio::spawn(async move { update_model::<Genre>(pool_clone, meili_client_clone).await });

    let _abort_guard = AbortOnDrop(vec![
        update_books_process.abort_handle(),
        update_authors_process.abort_handle(),
        update_sequences_process.abort_handle(),
        update_genres_process.abort_handle(),
    ]);

    let mut indices: Vec<IndexRunResult> = Vec::with_capacity(4);
    let mut any_failed = false;

    for (name, process) in [
        ("books", update_books_process),
        ("authors", update_authors_process),
        ("sequences", update_sequences_process),
        ("genres", update_genres_process),
    ] {
        let result = match process.await {
            Ok(Ok(count)) => {
                log::info!("Index update finished: index={} documents={}", name, count);
                IndexRunResult {
                    index: name.to_string(),
                    success: true,
                    document_count: Some(count),
                    error: None,
                }
            }
            Ok(Err(err)) => {
                any_failed = true;
                log::error!("Index update failed: index={} err={}", name, err);
                IndexRunResult {
                    index: name.to_string(),
                    success: false,
                    document_count: None,
                    error: Some(format!("{}", err)),
                }
            }
            Err(err) => {
                any_failed = true;
                log::error!("Index update failed: index={} err={:?}", name, err);
                IndexRunResult {
                    index: name.to_string(),
                    success: false,
                    document_count: None,
                    error: Some(format!("{:?}", err)),
                }
            }
        };

        indices.push(result);
    }

    let duration_ms = started.elapsed().as_millis() as u64;

    let summary = indices
        .iter()
        .map(|r| {
            format!(
                "{}={}",
                r.index,
                r.document_count
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "failed".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    if any_failed {
        log::error!(
            "Update run finished with failures: duration_ms={} {}",
            duration_ms,
            summary
        );
    } else {
        log::info!(
            "Update run finished: duration_ms={} {}",
            duration_ms,
            summary
        );
    }

    Ok(RunResult {
        started_at_unix,
        duration_ms,
        indices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_task(code: &str) -> Task {
        serde_json::from_str(&format!(
            r#"{{
              "uid": 1, "indexUid": "x", "status": "failed", "type": "indexDeletion",
              "details": {{"deletedDocuments": 0}},
              "duration": "PT0.1S",
              "enqueuedAt": "2022-02-03T13:02:38.369634Z",
              "startedAt": "2022-02-03T13:02:38.369634Z",
              "finishedAt": "2022-02-03T13:02:38.369634Z",
              "error": {{"message": "m", "code": "{code}", "type": "invalid_request", "link": "l"}}
            }}"#
        ))
        .unwrap()
    }

    #[test]
    fn classifies_index_not_found_task_failure() {
        assert!(is_index_not_found_failure(&failed_task("index_not_found")));
        assert!(!is_index_not_found_failure(&failed_task("internal")));
        assert!(ensure_task_succeeded(failed_task("internal"), "x").is_err());
    }

    #[test]
    fn publish_guard() {
        assert!(check_publish_allowed(0, 10, 0.5, false).is_err());
        assert!(check_publish_allowed(0, 10, 0.5, true).is_ok());
        assert!(check_publish_allowed(4, 10, 0.5, false).is_err());
        assert!(check_publish_allowed(5, 10, 0.5, false).is_ok());
        assert!(check_publish_allowed(1, 0, 0.5, false).is_ok());
    }
}
