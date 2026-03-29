fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[derive(Clone)]
pub struct Config {
    pub email: String,
    pub password: String,
    pub custom_id: String,
    pub base_url: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub retry_duration: u64,
    pub retry_interval: u64,
    pub discord_token: Option<String>,
    pub discord_owner_id: Option<u64>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("email", &self.email)
            .field("password", &"<redacted>")
            .field("custom_id", &self.custom_id)
            .field("base_url", &self.base_url)
            .field("latitude", &self.latitude)
            .field("longitude", &self.longitude)
            .field("retry_duration", &self.retry_duration)
            .field("retry_interval", &self.retry_interval)
            .field(
                "discord_token",
                &self.discord_token.as_ref().map(|_| "<redacted>"),
            )
            .field("discord_owner_id", &self.discord_owner_id)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            email: std::env::var("EMAIL").expect("EMAIL not set"),
            password: std::env::var("PASSWORD").expect("PASSWORD not set"),
            custom_id: std::env::var("CUSTOM_ID").expect("CUSTOM_ID not set"),
            base_url: std::env::var("BASE_URL").expect("BASE_URL not set"),
            latitude: std::env::var("LATITUDE").ok().and_then(|v| v.parse().ok()),
            longitude: std::env::var("LONGITUDE").ok().and_then(|v| v.parse().ok()),
            retry_duration: env_parse("RETRY_DURATION", 300),
            retry_interval: env_parse("RETRY_INTERVAL", 5),
            discord_token: std::env::var("DISCORD_TOKEN")
                .ok()
                .filter(|s| !s.is_empty()),
            discord_owner_id: std::env::var("DISCORD_OWNER_ID")
                .ok()
                .and_then(|v| v.parse().ok()),
        }
    }
}
