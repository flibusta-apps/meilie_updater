use deadpool_postgres::{Config, CreatePoolError, ManagerConfig, Pool, RecyclingMethod, Runtime};
use futures::{pin_mut, StreamExt};
use meilisearch_sdk::client::*;
use serde::Serialize;
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

async fn get_postgres_pool() -> Result<Pool, CreatePoolError> {
    let mut config = Config::new();

    config.host = Some(config::CONFIG.postgres_host.clone());
    config.port = Some(config::CONFIG.postgres_port);
    config.dbname = Some(config::CONFIG.postgres_db_name.clone());
    config.user = Some(config::CONFIG.postgres_user.clone());
    config.password = Some(config::CONFIG.postgres_password.clone());
    config.connect_timeout = Some(std::time::Duration::from_secs(5));
    config.manager = Some(ManagerConfig {
        recycling_method: RecyclingMethod::Verified,
    });

    match config.create_pool(Some(Runtime::Tokio1), NoTls) {
        Ok(pool) => Ok(pool),
        Err(err) => Err(err),
    }
}

fn get_meili_client() -> Result<Client, meilisearch_sdk::errors::Error> {
    Client::new(
        config::CONFIG.meili_host.clone(),
        Some(config::CONFIG.meili_master_key.clone()),
    )
}

async fn wait_for_meili_task(
    task_info: meilisearch_sdk::task_info::TaskInfo,
    client: &Client,
    context: &str,
) -> Result<(), Box<dyn std::error::Error + Send>> {
    let task = match task_info
        .wait_for_completion(client, None, Some(std::time::Duration::from_secs(120)))
        .await
    {
        Ok(task) => task,
        Err(err) => return Err(Box::new(err)),
    };

    if task.is_failure() {
        let failure = task.unwrap_failure();
        return Err(Box::new(std::io::Error::other(format!(
            "{context} task failed: {failure}"
        ))));
    }

    Ok(())
}

async fn update_model<T>(pool: Pool) -> Result<usize, Box<dyn std::error::Error + Send>>
where
    T: UpdateModel + Serialize + Send + Sync,
{
    let client = match pool.get().await {
        Ok(client) => client,
        Err(err) => return Err(Box::new(err)),
    };

    let meili_client = match get_meili_client() {
        Ok(client) => client,
        Err(err) => return Err(Box::new(err)),
    };

    let index = meili_client.index(T::get_index());

    let task_info = match index
        .set_searchable_attributes(T::get_searchable_attributes())
        .await
    {
        Ok(task_info) => task_info,
        Err(err) => return Err(Box::new(err)),
    };
    wait_for_meili_task(task_info, &meili_client, "set_searchable_attributes").await?;

    let task_info = match index
        .set_filterable_attributes(T::get_filterable_attributes())
        .await
    {
        Ok(task_info) => task_info,
        Err(err) => return Err(Box::new(err)),
    };
    wait_for_meili_task(task_info, &meili_client, "set_filterable_attributes").await?;

    let task_info = match index.set_ranking_rules(T::get_ranking_rules()).await {
        Ok(task_info) => task_info,
        Err(err) => return Err(Box::new(err)),
    };
    wait_for_meili_task(task_info, &meili_client, "set_ranking_rules").await?;

    let params: Vec<String> = vec![];
    let stream = match client.query_raw(&T::get_query(), params).await {
        Ok(stream) => stream,
        Err(err) => return Err(Box::new(err)),
    };

    pin_mut!(stream);
    let mut chunks = stream.chunks(1024);

    let mut total_count: usize = 0;

    while let Some(chunk) = chunks.next().await {
        let mut items: Vec<T> = Vec::with_capacity(chunk.len());

        for result in chunk.into_iter() {
            let row = match result {
                Ok(row) => row,
                Err(err) => return Err(Box::new(err)),
            };

            match T::from_row(row) {
                Ok(item) => items.push(item),
                Err(err) => return Err(Box::new(err)),
            }
        }

        total_count += items.len();

        let task_info = match index.add_or_update(&items, Some("id")).await {
            Ok(task_info) => task_info,
            Err(err) => return Err(Box::new(err)),
        };
        wait_for_meili_task(task_info, &meili_client, "add_or_update").await?;
    }

    Ok(total_count)
}

pub async fn update() -> Result<RunResult, Box<dyn std::error::Error>> {
    log::info!("Start update...");

    let pool = match get_postgres_pool().await {
        Ok(pool) => pool,
        Err(err) => return Err(Box::new(err)),
    };

    let started = std::time::Instant::now();
    let started_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let pool_clone = pool.clone();
    let update_books_process = tokio::spawn(async move { update_model::<Book>(pool_clone).await });

    let pool_clone = pool.clone();
    let update_authors_process =
        tokio::spawn(async move { update_model::<Author>(pool_clone).await });

    let pool_clone = pool.clone();
    let update_sequences_process =
        tokio::spawn(async move { update_model::<Sequence>(pool_clone).await });

    let pool_clone = pool.clone();
    let update_genres_process =
        tokio::spawn(async move { update_model::<Genre>(pool_clone).await });

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
