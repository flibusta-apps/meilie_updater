pub struct Config {
    pub api_key: String,

    pub sentry_sdn: String,

    pub postgres_db_name: String,
    pub postgres_host: String,
    pub postgres_port: u16,
    pub postgres_user: String,
    pub postgres_password: String,

    pub meili_host: String,
    pub meili_master_key: String
}

fn get_env(env: &'static str) -> String {
    std::env::var(env).unwrap_or_else(|_| panic!("Cannot get the {} env variable", env))
}

impl Config {
    pub fn load() -> Config {
        Config {
            api_key: get_env("API_KEY"),

            sentry_sdn: get_env("SENTRY_SDN"),

            postgres_db_name: get_env("POSTGRES_DB_NAME"),
            postgres_host: get_env("POSTGRES_HOST"),
            postgres_port: get_env("POSTGRES_PORT").parse().unwrap(),
            postgres_user: get_env("POSTGRES_USER"),
            postgres_password: get_env("POSTGRES_PASSWORD"),

            meili_host: get_env("MEILI_HOST"),
            meili_master_key: get_env("MEILI_MASTER_KEY")
        }
    }
}

lazy_static! {
    pub static ref CONFIG: Config = Config::load();
}
