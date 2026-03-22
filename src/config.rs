fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub struct Config {
    pub email: String,
    pub password: String,
    pub custom_id: String,
    pub base_url: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub retry_duration: u64,
    pub retry_interval: u64,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            email:          std::env::var("EMAIL").expect("EMAIL not set"),
            password:       std::env::var("PASSWORD").expect("PASSWORD not set"),
            custom_id:      std::env::var("CUSTOM_ID").expect("CUSTOM_ID not set"),
            base_url:       std::env::var("BASE_URL").expect("BASE_URL not set"),
            latitude:       std::env::var("LATITUDE").ok().and_then(|v| v.parse().ok()),
            longitude:      std::env::var("LONGITUDE").ok().and_then(|v| v.parse().ok()),
            retry_duration: env_parse("RETRY_DURATION", 300),
            retry_interval: env_parse("RETRY_INTERVAL", 5),
        }
    }
}
