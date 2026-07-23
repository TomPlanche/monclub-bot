use serde::Deserialize;

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Parse an optional env var, returning `None` when it is unset or unparseable.
fn env_opt<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}

/// Path (relative to the working directory) of the optional multi-user file.
const USERS_FILE: &str = "users.json";

/// One bookable `MonClub` identity: an account the bot can authenticate as and
/// book/cancel on behalf of. The primary account comes from the top-level
/// `EMAIL`/`PASSWORD`/`CUSTOM_ID` env vars; extra accounts come from the
/// `users.json` file.
#[derive(Clone)]
pub struct Account {
    /// Human-friendly name used in bot replies (e.g. "tom").
    pub label: String,
    pub email: String,
    pub password: String,
    pub custom_id: String,
    /// Discord user id this account is linked to, used to resolve `/book @tom`.
    pub discord_id: Option<u64>,
}

/// Whether two accounts authenticate as the same `MonClub` identity: the login
/// is keyed by email within a club, so same email (case-insensitively) plus same
/// `custom_id` means the same underlying member.
pub fn same_identity(a: &Account, b: &Account) -> bool {
    a.email.eq_ignore_ascii_case(&b.email) && a.custom_id == b.custom_id
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("label", &self.label)
            .field("email", &self.email)
            .field("password", &"<redacted>")
            .field("custom_id", &self.custom_id)
            .field("discord_id", &self.discord_id)
            .finish()
    }
}

/// One entry of the `users.json` array. `custom_id` defaults to the club-wide
/// `CUSTOM_ID` when omitted, since the club/tenant is normally shared.
#[derive(Debug, Deserialize)]
struct RawAccount {
    label: String,
    email: String,
    password: String,
    custom_id: Option<String>,
    discord_id: u64,
}

/// Load extra bookable accounts from `users.json` in the working directory.
///
/// Returns an empty list when the file is absent or blank (the common case for
/// setups that don't use multi-user). Panics with a clear message when the file
/// exists but cannot be read or parsed, so a misconfiguration fails loudly at
/// startup rather than silently dropping users.
fn load_users(default_custom_id: &str) -> Vec<Account> {
    let raw = match std::fs::read_to_string(USERS_FILE) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("failed to read {USERS_FILE}: {e}"),
    };

    if raw.trim().is_empty() {
        return Vec::new();
    }

    parse_users_json(&raw, default_custom_id).unwrap_or_else(|e| {
        panic!("{USERS_FILE} is not valid JSON (expected an array of user objects): {e}")
    })
}

/// Parse a `users.json` string into accounts, applying the default `custom_id`
/// to entries that omit one. Split out from [`load_users`] so the JSON shape can
/// be tested without touching the filesystem.
fn parse_users_json(raw: &str, default_custom_id: &str) -> serde_json::Result<Vec<Account>> {
    let parsed: Vec<RawAccount> = serde_json::from_str(raw)?;

    Ok(parsed
        .into_iter()
        .map(|r| Account {
            label: r.label,
            email: r.email,
            password: r.password,
            custom_id: r.custom_id.unwrap_or_else(|| default_custom_id.to_string()),
            discord_id: Some(r.discord_id),
        })
        .collect())
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
    /// Discord channel the new-session watcher posts to. When unset, the watcher
    /// is disabled.
    pub new_sessions_channel_id: Option<u64>,
    /// How often (seconds) the new-session watcher polls for newly available
    /// sessions.
    pub new_sessions_poll_interval: u64,
    /// How long (hours) before a session's start the booking window opens. Used
    /// by `/notify` to work out when a not-yet-bookable session becomes bookable.
    pub booking_window_hours: i64,
    /// How often (seconds) `/watchbook` re-attempts a booking once the window is
    /// open but the session is full, waiting for someone to unbook. Much slower
    /// than `retry_interval`, since this watch can run for days.
    pub watch_poll_interval: u64,
    /// Extra bookable accounts loaded from `users.json`, keyed to Discord users.
    /// These take precedence over the primary account when they share a Discord
    /// id or label (see [`Config::accounts`]).
    pub users: Vec<Account>,
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
            .field("new_sessions_channel_id", &self.new_sessions_channel_id)
            .field(
                "new_sessions_poll_interval",
                &self.new_sessions_poll_interval,
            )
            .field("booking_window_hours", &self.booking_window_hours)
            .field("watch_poll_interval", &self.watch_poll_interval)
            .field("users", &self.users)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Self {
        let custom_id = std::env::var("CUSTOM_ID").expect("CUSTOM_ID not set");
        let users = load_users(&custom_id);

        Self {
            email: std::env::var("EMAIL").expect("EMAIL not set"),
            password: std::env::var("PASSWORD").expect("PASSWORD not set"),
            custom_id,
            base_url: std::env::var("BASE_URL").expect("BASE_URL not set"),
            latitude: env_opt("LATITUDE"),
            longitude: env_opt("LONGITUDE"),
            retry_duration: env_parse("RETRY_DURATION", 300),
            retry_interval: env_parse("RETRY_INTERVAL", 5),
            discord_token: std::env::var("DISCORD_TOKEN")
                .ok()
                .filter(|s| !s.is_empty()),
            discord_owner_id: env_opt("DISCORD_OWNER_ID"),
            new_sessions_channel_id: env_opt("NEW_SESSIONS_CHANNEL_ID"),
            new_sessions_poll_interval: env_parse("NEW_SESSIONS_POLL_INTERVAL", 60),
            booking_window_hours: env_parse("BOOKING_WINDOW_HOURS", 144),
            watch_poll_interval: env_parse("WATCH_POLL_INTERVAL", 60),
            users,
        }
    }

    /// The primary account, built from the top-level `EMAIL`/`PASSWORD`/`CUSTOM_ID`
    /// env vars and linked to the Discord owner.
    pub fn primary_account(&self) -> Account {
        Account {
            label: "me".to_string(),
            email: self.email.clone(),
            password: self.password.clone(),
            custom_id: self.custom_id.clone(),
            discord_id: self.discord_owner_id,
        }
    }

    /// All bookable accounts in resolution order: `users.json` entries first,
    /// then the primary account. File entries come first so one that shares the
    /// owner's Discord id or label overrides the `EMAIL`/`PASSWORD` account; the
    /// primary account remains the fallback for anyone not in `users.json`.
    pub fn accounts(&self) -> Vec<Account> {
        let mut all = Vec::with_capacity(1 + self.users.len());
        all.extend(self.users.iter().cloned());
        all.push(self.primary_account());
        all
    }

    /// Bookable accounts with entries that resolve to the same `MonClub`
    /// identity (same email + club) collapsed to one, keeping the primary "me"
    /// entry so the same person is never listed twice in the picker. Used for
    /// display and to decide whether a choice is even needed.
    pub fn distinct_accounts(&self) -> Vec<Account> {
        // Primary first here so it wins the identity de-duplication (a user who
        // also appears as themselves in users.json shows once, as "me").
        let mut ordered = Vec::with_capacity(1 + self.users.len());
        ordered.push(self.primary_account());
        ordered.extend(self.users.iter().cloned());

        let mut distinct: Vec<Account> = Vec::new();
        for account in ordered {
            let duplicate = distinct.iter().any(|kept| same_identity(kept, &account));
            if !duplicate {
                distinct.push(account);
            }
        }
        distinct
    }

    /// Find the account linked to a given Discord user id, if any.
    pub fn account_for_discord(&self, discord_id: u64) -> Option<Account> {
        self.accounts()
            .into_iter()
            .find(|a| a.discord_id == Some(discord_id))
    }

    /// Find the account with a given case-insensitive label, if any.
    pub fn account_for_label(&self, label: &str) -> Option<Account> {
        self.accounts()
            .into_iter()
            .find(|a| a.label.eq_ignore_ascii_case(label))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_users(users: Vec<Account>) -> Config {
        Config {
            email: "owner@example.com".to_string(),
            password: "ownerpwd".to_string(),
            custom_id: "club1".to_string(),
            base_url: "https://api.example.com".to_string(),
            latitude: None,
            longitude: None,
            retry_duration: 300,
            retry_interval: 5,
            discord_token: None,
            discord_owner_id: Some(1),
            new_sessions_channel_id: None,
            new_sessions_poll_interval: 60,
            booking_window_hours: 144,
            watch_poll_interval: 60,
            users,
        }
    }

    #[test]
    fn parses_users_and_defaults_custom_id() {
        let raw = r#"[
            {"discord_id": 111, "label": "tom", "email": "tom@x.com", "password": "p1"},
            {"discord_id": 222, "label": "nils", "email": "nils@x.com", "password": "p2", "custom_id": "club2"}
        ]"#;

        let users = parse_users_json(raw, "club1").expect("valid JSON");

        assert_eq!(users.len(), 2);
        assert_eq!(users[0].discord_id, Some(111));
        // custom_id omitted -> falls back to the club-wide default.
        assert_eq!(users[0].custom_id, "club1");
        // custom_id present -> kept as-is.
        assert_eq!(users[1].custom_id, "club2");
    }

    #[test]
    fn preserves_large_snowflake_ids() {
        // Discord ids exceed 2^53, so they must survive JSON parsing exactly.
        let raw = r#"[{"discord_id": 123456789012345678, "label": "t", "email": "t@x.com", "password": "p"}]"#;
        let users = parse_users_json(raw, "club1").expect("valid JSON");
        assert_eq!(users[0].discord_id, Some(123_456_789_012_345_678));
    }

    #[test]
    fn rejects_malformed_users_json() {
        assert!(parse_users_json("not json", "club1").is_err());
    }

    #[test]
    fn resolves_owner_and_users_by_discord_id() {
        let users = parse_users_json(
            r#"[{"discord_id": 111, "label": "tom", "email": "tom@x.com", "password": "p1"}]"#,
            "club1",
        )
        .unwrap();
        let config = config_with_users(users);

        // Owner id resolves to the primary account.
        assert_eq!(
            config.account_for_discord(1).unwrap().email,
            "owner@example.com"
        );
        // A USERS entry resolves to its own account.
        assert_eq!(config.account_for_discord(111).unwrap().label, "tom");
        // Unknown id resolves to nothing.
        assert!(config.account_for_discord(999).is_none());
    }

    #[test]
    fn resolves_by_case_insensitive_label() {
        let users = parse_users_json(
            r#"[{"discord_id": 111, "label": "Tom", "email": "tom@x.com", "password": "p1"}]"#,
            "club1",
        )
        .unwrap();
        let config = config_with_users(users);

        assert_eq!(
            config.account_for_label("tom").unwrap().discord_id,
            Some(111)
        );
        assert!(config.account_for_label("unknown").is_none());
    }

    #[test]
    fn distinct_accounts_collapses_duplicate_identity() {
        // A users.json entry for the owner's own email (owner@example.com,
        // matching config_with_users) is the same identity as the primary.
        let users = parse_users_json(
            r#"[{"discord_id": 9, "label": "tom", "email": "owner@example.com", "password": "p"}]"#,
            "club1",
        )
        .unwrap();
        let config = config_with_users(users);

        // Non-distinct list keeps both; distinct collapses to the primary "me".
        assert_eq!(config.accounts().len(), 2);
        let distinct = config.distinct_accounts();
        assert_eq!(distinct.len(), 1);
        assert_eq!(distinct[0].label, "me");
    }

    #[test]
    fn distinct_accounts_keeps_genuinely_different_users() {
        let users = parse_users_json(
            r#"[{"discord_id": 2, "label": "tom", "email": "tom@example.com", "password": "p"}]"#,
            "club1",
        )
        .unwrap();
        let config = config_with_users(users);

        assert_eq!(config.distinct_accounts().len(), 2);
    }

    #[test]
    fn users_entry_overrides_primary_on_discord_id() {
        // A USERS entry sharing the owner's discord id (1) must win over the
        // EMAIL/PASSWORD primary account.
        let users = parse_users_json(
            r#"[{"discord_id": 1, "label": "owner-alt", "email": "alt@x.com", "password": "p"}]"#,
            "club1",
        )
        .unwrap();
        let config = config_with_users(users);

        let resolved = config.account_for_discord(1).unwrap();
        assert_eq!(resolved.email, "alt@x.com");
        assert_eq!(resolved.label, "owner-alt");
    }

    #[test]
    fn load_users_reads_file_and_handles_absence() {
        use std::fs;

        // Never clobber a real users.json a developer may have locally.
        if std::path::Path::new(USERS_FILE).exists() {
            return;
        }

        // Absent file -> no extra users.
        assert!(load_users("club1").is_empty());

        // Present file -> parsed, with custom_id defaulted from the argument.
        fs::write(
            USERS_FILE,
            r#"[{"discord_id":7,"label":"z","email":"z@x.com","password":"p"}]"#,
        )
        .unwrap();
        let loaded = load_users("club1");
        fs::remove_file(USERS_FILE).ok();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].discord_id, Some(7));
        assert_eq!(loaded[0].custom_id, "club1");
    }

    #[test]
    fn users_entry_overrides_primary_on_label() {
        // A USERS entry using the primary's "me" label must win too.
        let users = parse_users_json(
            r#"[{"discord_id": 5, "label": "me", "email": "alt@x.com", "password": "p"}]"#,
            "club1",
        )
        .unwrap();
        let config = config_with_users(users);

        assert_eq!(config.account_for_label("me").unwrap().email, "alt@x.com");
    }
}
