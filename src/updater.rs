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
    pub outcome: String,
    pub error: Option<String>,
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
        Task::Succeeded { .. } => Ok(()),
        Task::Failed { content } => Err(boxed(std::io::Error::other(format!(
            "{context} task failed: {}",
            content.error
        )))),
        task => Err(boxed(std::io::Error::other(format!(
            "{context} task did not succeed: {task:?}"
        )))),
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

async fn update_model<T>(
    client: &deadpool_postgres::Transaction<'_>,
    meili_client: &MeiliClient,
    staging_uid: &str,
) -> Result<usize, BoxError>
where
    T: UpdateModel + Serialize + Send + Sync,
{
    let live_uid = T::get_index();

    // `swap_indexes` requires both index UIDs to already exist, so bootstrap an
    // empty live index on the very first run.
    match meili_client.get_index(&live_uid).await {
        Ok(_) => (),
        Err(MeiliError::Meilisearch(err)) if err.error_code == ErrorCode::IndexNotFound => {
            let task_info = match meili_client.create_index(&live_uid, Some("id")).await {
                Ok(task_info) => task_info,
                Err(err) => return Err(Box::new(err)),
            };
            wait_for_meili_task(task_info, meili_client, "create_index (live)").await?;
        }
        Err(err) => return Err(Box::new(err)),
    };

    // Create the staging index and build the new dataset there.
    let task_info = match meili_client.create_index(&staging_uid, Some("id")).await {
        Ok(task_info) => task_info,
        Err(err) => return Err(Box::new(err)),
    };
    wait_for_meili_task(task_info, meili_client, "create_index (staging)").await?;

    let staging_index = meili_client.index(staging_uid);

    let task_info = match staging_index
        .set_searchable_attributes(T::get_searchable_attributes())
        .await
    {
        Ok(task_info) => task_info,
        Err(err) => return Err(Box::new(err)),
    };
    wait_for_meili_task(task_info, meili_client, "set_searchable_attributes").await?;

    let task_info = match staging_index
        .set_filterable_attributes(T::get_filterable_attributes())
        .await
    {
        Ok(task_info) => task_info,
        Err(err) => return Err(Box::new(err)),
    };
    wait_for_meili_task(task_info, meili_client, "set_filterable_attributes").await?;

    let task_info = match staging_index
        .set_ranking_rules(T::get_ranking_rules())
        .await
    {
        Ok(task_info) => task_info,
        Err(err) => return Err(Box::new(err)),
    };
    wait_for_meili_task(task_info, meili_client, "set_ranking_rules").await?;

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
        let task = wait_task(last, meili_client, timeout).await?;
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
        return Err(boxed(std::io::Error::other(format!(
            "refusing to publish index {live_uid}: {reason}"
        ))));
    }

    Ok(total_count)
}

// Stable across replicas. The connection is detached from the pool so dropping
// it always closes the session (and releases the advisory lock), even on panic.
const UPDATE_LOCK: i64 = 0x6d65696c695f7570;
const INDEX_NAMES: [&str; 4] = ["books", "authors", "sequences", "genres"];

fn is_staging_uid(uid: &str) -> bool {
    INDEX_NAMES
        .iter()
        .any(|name| uid == format!("{name}_new") || uid.starts_with(&format!("{name}_new_")))
}

async fn clean_stale_indexes(client: &MeiliClient) -> Result<(), BoxError> {
    // Fetch all pages before deleting, otherwise offsets shift while paginating.
    let mut offset = 0;
    let mut stale = Vec::new();
    loop {
        let mut query = meilisearch_sdk::indexes::IndexesQuery::new(client);
        query.with_offset(offset).with_limit(100);
        let page = client.list_all_indexes_with(&query).await.map_err(boxed)?;
        for index in &page.results {
            if is_staging_uid(&index.uid) {
                stale.push(index.uid.clone());
            }
        }
        offset += page.results.len();
        if offset >= page.total as usize || page.results.is_empty() {
            break;
        }
    }
    for uid in stale {
        delete_index_if_exists(client, &uid).await?;
    }
    Ok(())
}

async fn build_generation(
    connection: &mut deadpool_postgres::ClientWrapper,
    client: &MeiliClient,
    staging: &[String],
) -> Result<Vec<IndexRunResult>, BoxError> {
    clean_stale_indexes(client).await?;
    let transaction = connection
        .build_transaction()
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(boxed)?;
    // query_raw can remain active while we wait on Meilisearch HTTP. Give each
    // streaming statement the whole run budget, not the short session default.
    let timeout_ms = config::CONFIG.update_timeout_secs.saturating_mul(1000);
    transaction
        .execute(
            "SELECT set_config('statement_timeout', $1, true)",
            &[&timeout_ms.to_string()],
        )
        .await
        .map_err(boxed)?;
    let counts = [
        update_model::<Book>(&transaction, client, &staging[0]).await?,
        update_model::<Author>(&transaction, client, &staging[1]).await?,
        update_model::<Sequence>(&transaction, client, &staging[2]).await?,
        update_model::<Genre>(&transaction, client, &staging[3]).await?,
    ];
    transaction.commit().await.map_err(boxed)?;
    let swaps: Vec<_> = INDEX_NAMES
        .iter()
        .zip(staging)
        .map(|(live, new)| SwapIndexes {
            indexes: ((*live).to_string(), new.clone()),
        })
        .collect();
    let task = client.swap_indexes(&swaps).await.map_err(boxed)?;
    wait_for_meili_task(task, client, "swap_indexes (all indexes)").await?;
    Ok(INDEX_NAMES
        .iter()
        .zip(counts)
        .map(|(name, count)| IndexRunResult {
            index: (*name).to_string(),
            success: true,
            document_count: Some(count),
            error: None,
        })
        .collect())
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

    let timeout = Duration::from_secs(config::CONFIG.update_timeout_secs);
    let connection = tokio::time::timeout(timeout, pool.get()).await??;
    let mut connection = deadpool_postgres::Object::take(connection);
    let acquired: bool = connection
        .query_one("SELECT pg_try_advisory_lock($1)", &[&UPDATE_LOCK])
        .await?
        .get(0);
    if !acquired {
        return Ok(RunResult {
            started_at_unix,
            duration_ms: started.elapsed().as_millis() as u64,
            outcome: "busy".into(),
            error: Some("another replica holds the update lock".into()),
            indices: Vec::new(),
        });
    }
    // txid_current is cluster-wide unique; timestamp also separates cluster resets.
    let run_id: i64 = connection
        .query_one("SELECT txid_current()", &[])
        .await?
        .get(0);
    let staging: Vec<_> = INDEX_NAMES
        .iter()
        .map(|name| format!("{name}_new_{started_at_unix}_{run_id}"))
        .collect();
    let remaining = timeout.saturating_sub(started.elapsed());
    let result = tokio::time::timeout(
        remaining,
        build_generation(&mut connection, &meili_client, &staging),
    )
    .await;
    let (outcome, error, indices) = match result {
        Ok(Ok(indices)) => ("success", None, indices),
        Ok(Err(err)) => ("failed", Some(err.to_string()), Vec::new()),
        Err(_) => (
            "timeout",
            Some(format!("update run timed out after {timeout:?}")),
            Vec::new(),
        ),
    };
    // Keep the lock through cleanup, including after timeout. Failed HTTP/task
    // waits can leave queued tasks; deletion follows them in Meili's task queue.
    for uid in &staging {
        if let Err(err) = delete_index_if_exists(&meili_client, uid).await {
            log::error!("Failed to clean staging index {uid}: {err}");
        }
    }
    Ok(RunResult {
        started_at_unix,
        duration_ms: started.elapsed().as_millis() as u64,
        outcome: outcome.into(),
        error,
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

    #[test]
    fn staging_cleanup_is_scoped_to_owned_names() {
        for uid in [
            "books_new",
            "authors_new_123_456",
            "sequences_new_1",
            "genres_new_2",
        ] {
            assert!(is_staging_uid(uid), "{uid}");
        }
        for uid in [
            "books",
            "authors",
            "books_news",
            "other_new_1",
            "my_books_new_1",
        ] {
            assert!(!is_staging_uid(uid), "{uid}");
        }
    }
}
