pub struct Config {
    pub api_key: String,

    pub sentry_dsn: String,

    pub postgres_db_name: String,
    pub postgres_host: String,
    pub postgres_port: u16,
    pub postgres_user: String,
    pub postgres_password: String,

    pub meili_host: String,
    pub meili_master_key: String,

    /// Postgres session-level `statement_timeout`, in milliseconds.
    /// Env: `STATEMENT_TIMEOUT_MS`, default `300000`.
    pub statement_timeout_ms: u64,
    /// Maximum number of connections in the Postgres pool.
    /// Env: `POOL_MAX_SIZE`, default `8`.
    pub pool_max_size: usize,
    /// How long to wait for a free pool connection before giving up.
    /// Env: `POOL_WAIT_TIMEOUT_SECS`, default `5`.
    pub pool_wait_timeout_secs: u64,
    /// How long to wait when creating a new pool connection before giving up.
    /// Env: `POOL_CREATE_TIMEOUT_SECS`, default `5`.
    pub pool_create_timeout_secs: u64,
    /// How long to wait when recycling a pool connection before giving up.
    /// Env: `POOL_RECYCLE_TIMEOUT_SECS`, default `5`.
    pub pool_recycle_timeout_secs: u64,
    /// Number of rows streamed from Postgres per Meilisearch `add_or_update`
    /// batch. Env: `BATCH_SIZE`, default `1024`.
    pub batch_size: usize,
}

fn get_env(env: &'static str) -> String {
    std::env::var(env).unwrap_or_else(|_| panic!("Cannot get the {} env variable", env))
}

fn get_env_with_default<T: std::str::FromStr>(env: &'static str, default: T) -> T {
    std::env::var(env)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

impl Config {
    pub fn load() -> Config {
        Config {
            api_key: get_env("API_KEY"),

            sentry_dsn: get_env("SENTRY_DSN"),

            postgres_db_name: get_env("POSTGRES_DB_NAME"),
            postgres_host: get_env("POSTGRES_HOST"),
            postgres_port: get_env("POSTGRES_PORT").parse().unwrap(),
            postgres_user: get_env("POSTGRES_USER"),
            postgres_password: get_env("POSTGRES_PASSWORD"),

            meili_host: get_env("MEILI_HOST"),
            meili_master_key: get_env("MEILI_MASTER_KEY"),

            statement_timeout_ms: get_env_with_default("STATEMENT_TIMEOUT_MS", 300_000),
            pool_max_size: get_env_with_default("POOL_MAX_SIZE", 8),
            pool_wait_timeout_secs: get_env_with_default("POOL_WAIT_TIMEOUT_SECS", 5),
            pool_create_timeout_secs: get_env_with_default("POOL_CREATE_TIMEOUT_SECS", 5),
            pool_recycle_timeout_secs: get_env_with_default("POOL_RECYCLE_TIMEOUT_SECS", 5),
            batch_size: get_env_with_default("BATCH_SIZE", 1024),
        }
    }
}

lazy_static! {
    pub static ref CONFIG: Config = Config::load();
}
