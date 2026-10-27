use std::str::FromStr;
use std::sync::LazyLock;

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
    /// Timeout of a single HTTP request to Meilisearch.
    /// Env: `MEILI_REQUEST_TIMEOUT_SECS`, default `120`.
    pub meili_request_timeout_secs: u64,
    /// Timeout of the whole update run. Env: `UPDATE_TIMEOUT_SECS`, default `10800` (3h).
    pub update_timeout_secs: u64,
    /// Maximum allowed shrink of an index (0..=1) relative to the live one.
    /// Env: `MAX_SHRINK_RATIO`, default `0.5`.
    pub max_shrink_ratio: f64,
    /// Allow publishing empty / strongly shrunk indexes.
    /// Env: `ALLOW_EMPTY_INDEX`, default `false`.
    pub allow_empty_index: bool,
}

fn required(get: &impl Fn(&str) -> Option<String>, name: &str) -> Result<String, String> {
    get(name).ok_or_else(|| format!("Cannot get the {name} env variable"))
}

fn required_parsed<T: FromStr>(
    get: &impl Fn(&str) -> Option<String>,
    name: &str,
) -> Result<T, String> {
    let raw = required(get, name)?;
    raw.trim()
        .parse()
        .map_err(|_| format!("Cannot parse the {name} env variable: {raw:?}"))
}

fn parsed_or<T: FromStr>(
    get: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: T,
) -> Result<T, String> {
    match get(name) {
        None => Ok(default),
        Some(raw) => raw
            .trim()
            .parse()
            .map_err(|_| format!("Cannot parse the {name} env variable: {raw:?}")),
    }
}

impl Config {
    pub fn load() -> Result<Config, String> {
        Self::load_from(|name| std::env::var(name).ok())
    }

    pub fn load_from(get: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        let api_key = required(&get, "API_KEY")?;
        if api_key.trim().is_empty() {
            return Err("API_KEY must not be empty".to_string());
        }

        let pool_max_size: usize = parsed_or(&get, "POOL_MAX_SIZE", 8)?;
        if pool_max_size == 0 {
            return Err("POOL_MAX_SIZE must be greater than 0".to_string());
        }

        let batch_size: usize = parsed_or(&get, "BATCH_SIZE", 1024)?;
        if batch_size == 0 {
            return Err("BATCH_SIZE must be greater than 0".to_string());
        }

        let update_timeout_secs: u64 = parsed_or(&get, "UPDATE_TIMEOUT_SECS", 3 * 60 * 60)?;
        if update_timeout_secs == 0 {
            return Err("UPDATE_TIMEOUT_SECS must be greater than 0".to_string());
        }

        let meili_request_timeout_secs: u64 = parsed_or(&get, "MEILI_REQUEST_TIMEOUT_SECS", 120)?;
        if meili_request_timeout_secs == 0 {
            return Err("MEILI_REQUEST_TIMEOUT_SECS must be greater than 0".to_string());
        }

        let max_shrink_ratio: f64 = parsed_or(&get, "MAX_SHRINK_RATIO", 0.5)?;
        if !(0.0..=1.0).contains(&max_shrink_ratio) {
            return Err("MAX_SHRINK_RATIO must be between 0 and 1".to_string());
        }

        let allow_empty_index = match get("ALLOW_EMPTY_INDEX") {
            None => false,
            Some(v) => match v.trim().to_ascii_lowercase().as_str() {
                "true" | "1" => true,
                "false" | "0" | "" => false,
                _ => {
                    return Err(format!(
                        "Cannot parse the ALLOW_EMPTY_INDEX env variable: {v:?}"
                    ))
                }
            },
        };

        Ok(Config {
            api_key,

            sentry_dsn: required(&get, "SENTRY_DSN")?,

            postgres_db_name: required(&get, "POSTGRES_DB_NAME")?,
            postgres_host: required(&get, "POSTGRES_HOST")?,
            postgres_port: required_parsed(&get, "POSTGRES_PORT")?,
            postgres_user: required(&get, "POSTGRES_USER")?,
            postgres_password: required(&get, "POSTGRES_PASSWORD")?,

            meili_host: required(&get, "MEILI_HOST")?,
            meili_master_key: required(&get, "MEILI_MASTER_KEY")?,

            statement_timeout_ms: parsed_or(&get, "STATEMENT_TIMEOUT_MS", 300_000)?,
            pool_max_size,
            pool_wait_timeout_secs: parsed_or(&get, "POOL_WAIT_TIMEOUT_SECS", 5)?,
            pool_create_timeout_secs: parsed_or(&get, "POOL_CREATE_TIMEOUT_SECS", 5)?,
            pool_recycle_timeout_secs: parsed_or(&get, "POOL_RECYCLE_TIMEOUT_SECS", 5)?,
            batch_size,
            meili_request_timeout_secs,
            update_timeout_secs,
            max_shrink_ratio,
            allow_empty_index,
        })
    }
}

pub static CONFIG: LazyLock<Config> =
    LazyLock::new(|| Config::load().unwrap_or_else(|err| panic!("Invalid configuration: {err}")));

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn base() -> HashMap<&'static str, String> {
        HashMap::from([
            ("API_KEY", "key".to_string()),
            ("SENTRY_DSN", "https://public@example.com/1".to_string()),
            ("POSTGRES_DB_NAME", "db".to_string()),
            ("POSTGRES_HOST", "h".to_string()),
            ("POSTGRES_PORT", "5432".to_string()),
            ("POSTGRES_USER", "u".to_string()),
            ("POSTGRES_PASSWORD", "p".to_string()),
            ("MEILI_HOST", "http://m".to_string()),
            ("MEILI_MASTER_KEY", "k".to_string()),
        ])
    }

    fn load(env: &HashMap<&'static str, String>) -> Result<Config, String> {
        Config::load_from(|n| env.get(n).cloned())
    }

    #[test]
    fn defaults_are_valid() {
        let cfg = load(&base()).unwrap();
        assert_eq!(cfg.batch_size, 1024);
        assert!(!cfg.allow_empty_index);
    }

    #[test]
    fn rejects_bad_values() {
        for (k, v) in [
            ("API_KEY", "  "),
            ("BATCH_SIZE", "0"),
            ("BATCH_SIZE", "abc"),
            ("POOL_MAX_SIZE", "0"),
            ("POOL_WAIT_TIMEOUT_SECS", "x"),
            ("MAX_SHRINK_RATIO", "2"),
            ("ALLOW_EMPTY_INDEX", "maybe"),
            ("POSTGRES_PORT", "nope"),
        ] {
            let mut env = base();
            env.insert(k, v.to_string());
            assert!(load(&env).is_err(), "{k}={v} should fail");
        }
    }
}
