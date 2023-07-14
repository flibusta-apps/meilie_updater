use deadpool_postgres::{Config, CreatePoolError, ManagerConfig, Pool, RecyclingMethod, Runtime};
use tokio_postgres::NoTls;
use futures::{StreamExt, pin_mut};
use meilisearch_sdk::client::*;
use serde::Serialize;

use crate::{config, models::{Book, UpdateModel, Author, Sequence, Genre}};

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

fn get_meili_client() -> Client {
    Client::new(config::CONFIG.meili_host.clone(), Some(config::CONFIG.meili_master_key.clone()))
}

async fn update_model<T>(
    pool: Pool,
) -> Result<(), Box<dyn std::error::Error + Send>>
where
    T: UpdateModel + Serialize
{
    let client = pool.get().await.unwrap();

    let meili_client = get_meili_client();

    let index = meili_client.index(T::get_index());

    let params: Vec<String> = vec![];
    let stream = match client.query_raw(
        &T::get_query(), params
    ).await {
        Ok(stream) => stream,
        Err(err) => return Err(Box::new(err)),
    };

    pin_mut!(stream);
    let mut chunks = stream.chunks(1024);

    while let Some(chunk) = chunks.next().await {
        let items: Vec<T> = chunk
            .into_iter()
            .map(|result| {
                match result {
                    Ok(v) => T::from_row(v),
                    Err(err) => panic!("{}", err),
                }
            })
            .collect();

        if let Err(err) = index.add_or_update(&items, Some("id")).await {
            return Err(Box::new(err));
        };
    }

    if let Err(err) = index.set_searchable_attributes(T::get_searchanble_attributes()).await {
        return Err(Box::new(err));
    };

    if let Err(err) = index.set_filterable_attributes(T::get_filterable_attributes()).await {
        return Err(Box::new(err));
    };

    if let Err(err) = index.set_ranking_rules(T::get_ranking_rules()).await {
        return Err(Box::new(err));
    };

    Ok(())
}

pub async fn update() -> Result<(), Box<dyn std::error::Error>> {
    log::info!("Start update...");

    let pool = match get_postgres_pool().await {
        Ok(pool) => pool,
        Err(err) => panic!("{:?}", err),
    };

    let pool_clone = pool.clone();
    let update_books_process = tokio::spawn(async move {
        match update_model::<Book>(pool_clone).await {
            Ok(_) => (),
            Err(err) => panic!("{}", err),
        }
    });

    let pool_clone = pool.clone();
    let update_authors_process = tokio::spawn(async move {
        match update_model::<Author>(pool_clone).await {
            Ok(_) => (),
            Err(err) => panic!("{}", err),
        }
    });

    let pool_clone = pool.clone();
    let update_sequences_process = tokio::spawn(async move {
        match update_model::<Sequence>(pool_clone).await  {
            Ok(_) => (),
            Err(err) => panic!("{}", err),
        }
    });

    let pool_clone = pool.clone();
    let update_genres_process = tokio::spawn(async move {
        match update_model::<Genre>(pool_clone).await  {
            Ok(_) => (),
            Err(err) => panic!("{}", err),
        }
    });

    for process in [
        update_books_process,
        update_authors_process,
        update_sequences_process,
        update_genres_process
    ] {
        match process.await {
            Ok(v) => v,
            Err(err) => return Err(Box::new(err)),
        };
    }

    Ok(())
}
