use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local};
use monclub_bot::client::{
    BookError, Booking, MonClubClient, Session, SessionComparison, SessionDetail, parse_when,
};
use monclub_bot::config::{Account, Config, same_identity};
use monclub_bot::logging;
use poise::serenity_prelude::{self as serenity, AutocompleteChoice};
use tracing::info;

struct Data {
    config: Config,
    owner_id: Option<serenity::UserId>,
}

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

fn is_owner(ctx: &Context<'_>) -> bool {
    match ctx.data().owner_id {
        None => true,
        Some(owner_id) => ctx.author().id == owner_id,
    }
}

/// Reject a command with an "Unauthorized." reply unless the caller is the owner.
macro_rules! ensure_owner {
    ($ctx:expr) => {
        if !is_owner(&$ctx) {
            $ctx.say("Unauthorized.").await?;
            return Ok(());
        }
    };
}

/// Build an authenticated client on the blocking pool and run `f` against it.
///
/// Every command repeats the same three steps (new client, authenticate,
/// call); this centralises them and folds the join error into the result.
async fn with_client<T, F>(config: Config, f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&MonClubClient) -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut client = MonClubClient::new(config);
        client.authenticate()?;
        f(&client)
    })
    .await?
}

/// Like [`with_client`], but authenticates as a specific `account` rather than
/// the primary one. Used to book/cancel on behalf of other linked users.
async fn with_account<T, F>(config: Config, account: Account, f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&MonClubClient) -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut client = MonClubClient::with_account(config, account);
        client.authenticate()?;
        f(&client)
    })
    .await?
}

/// Extract a Discord user id from a mention token like `<@123>` or `<@!123>`.
fn parse_mention(token: &str) -> Option<u64> {
    let inner = token.strip_prefix("<@")?.strip_suffix('>')?;
    let inner = inner.strip_prefix('!').unwrap_or(inner);
    inner.parse().ok()
}

/// Whether a token means "every configured account": `@everyone`, `everyone`,
/// or `all` (case-insensitive).
fn is_everyone(token: &str) -> bool {
    let t = token.trim_start_matches('@');
    t.eq_ignore_ascii_case("everyone") || t.eq_ignore_ascii_case("all")
}

/// Resolve the optional `users` argument to the accounts a command should act
/// on.
///
/// When `users` is absent or blank, targets the caller's own linked account,
/// falling back to the primary account when the caller is not explicitly
/// linked. `@everyone` (or `everyone`/`all`) targets every configured account.
/// Otherwise each whitespace-separated token is resolved as a Discord mention
/// (`<@id>`), a raw Discord id, or an account label. Duplicates (same
/// `MonClub` identity) are removed. Returns an error naming any unresolved token.
fn resolve_targets(ctx: &Context<'_>, users: Option<&str>) -> Result<Vec<Account>, String> {
    let config = &ctx.data().config;

    let tokens: Vec<&str> = users
        .map(|s| s.split_whitespace().collect())
        .unwrap_or_default();

    if tokens.is_empty() {
        let caller = ctx.author().id.get();
        let account = config
            .account_for_discord(caller)
            .unwrap_or_else(|| config.primary_account());
        return Ok(vec![account]);
    }

    // `@everyone` expands to every distinct configured account.
    if tokens.iter().any(|t| is_everyone(t)) {
        return Ok(config.distinct_accounts());
    }

    let mut resolved: Vec<Account> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();

    for token in tokens {
        let account = parse_mention(token)
            .or_else(|| token.parse::<u64>().ok())
            .and_then(|id| config.account_for_discord(id))
            .or_else(|| config.account_for_label(token));

        match account {
            // De-duplicate when the same person is named twice.
            Some(a) if !resolved.iter().any(|r| same_identity(r, &a)) => resolved.push(a),
            Some(_) => {}
            None => unresolved.push(token.to_string()),
        }
    }

    if !unresolved.is_empty() {
        return Err(format!(
            "No linked MonClub account for: {}",
            unresolved.join(", ")
        ));
    }

    Ok(resolved)
}

/// Retry booking `session_id` in the background until the slot opens or the
/// deadline passes, posting the outcome to `channel_id`.
async fn retry_book(
    http: Arc<serenity::Http>,
    channel_id: serenity::ChannelId,
    config: Config,
    account: Account,
    session_id: String,
    retry_duration: u64,
    retry_interval: u64,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(retry_duration);
    let label = account.label.clone();

    loop {
        let sid = session_id.clone();
        let result = with_account(config.clone(), account.clone(), move |c| {
            Ok(c.book_session(&Session::from_id(sid)))
        })
        .await;

        match result {
            Ok(Ok(_)) => {
                let _ = channel_id
                    .say(&http, format!("Booked `{session_id}` for {label}."))
                    .await;
                return;
            }
            Ok(Err(BookError::SlotNotOpen(_))) => {
                if tokio::time::Instant::now() >= deadline {
                    let _ = channel_id
                        .say(&http, format!("Booking window expired for {label}."))
                        .await;
                    return;
                }
                tokio::time::sleep(Duration::from_secs(retry_interval)).await;
            }
            // A rejection (e.g. no credits) won't clear by retrying: stop and
            // report its message.
            Ok(Err(e @ BookError::Rejected { .. })) => {
                let _ = channel_id
                    .say(&http, format!("Booking failed for {label}: {e}"))
                    .await;
                return;
            }
            _ => {
                let _ = channel_id
                    .say(
                        &http,
                        format!("Booking failed for {label} with an unexpected error."),
                    )
                    .await;
                return;
            }
        }
    }
}

/// Poll for newly available sessions and announce them in `channel_id`.
///
/// Runs forever on its own task. The first poll seeds the set of known session
/// ids without posting, so the channel isn't flooded with the entire current
/// listing at startup; only sessions that appear in a later poll are announced.
async fn watch_new_sessions(
    http: Arc<serenity::Http>,
    channel_id: serenity::ChannelId,
    config: Config,
    poll_interval: u64,
) {
    use std::collections::HashSet;

    let interval = Duration::from_secs(poll_interval.max(1));
    let mut known: Option<HashSet<String>> = None;

    if let Err(e) = channel_id.say(&http, "Looking for new sessions...").await {
        tracing::warn!("Failed to post watcher start message: {e}");
    }

    loop {
        let sessions = with_client(config.clone(), MonClubClient::list_sessions).await;

        match sessions {
            Ok(sessions) => {
                let current: HashSet<String> = sessions.iter().map(|s| s.id.clone()).collect();

                match &known {
                    // First successful poll: remember what's already there and
                    // stay quiet.
                    None => {
                        info!(count = current.len(), "Seeded new-session watcher");
                        known = Some(current);
                    }
                    Some(previous) => {
                        let mut fresh: Vec<&Session> =
                            sessions.iter().filter(|s| !previous.contains(&s.id)).collect();
                        fresh.sort_by(|a, b| a.date.cmp(&b.date));

                        for s in fresh {
                            let msg = format!(
                                "New session available: {} (`{}`)",
                                format_session(s),
                                s.id
                            );
                            if let Err(e) = channel_id.say(&http, msg).await {
                                tracing::warn!("Failed to post new session: {e}");
                            }
                        }

                        known = Some(current);
                    }
                }
            }
            Err(e) => tracing::warn!("New-session watcher poll failed: {e}"),
        }

        tokio::time::sleep(interval).await;
    }
}

/// Parse a session `date` field into a local `DateTime`.
///
/// The API returns the session's start instant as an RFC 3339 UTC timestamp
/// (e.g. `2026-03-22T08:00:00.000Z`), so the local start time is derived from
/// that instant rather than the separate French `time` string (`09H00`).
fn parse_session_start(date: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(date)
        .ok()
        .map(|dt| dt.with_timezone(&Local))
}

/// Wait until `wait` elapses, then ping `user_id` in `channel_id` to say the
/// session is now bookable. Spawned by `/notify`; if the bot restarts before the
/// window opens, the pending alert is lost (same limitation as `/prebook`).
async fn alert_when_bookable(
    http: Arc<serenity::Http>,
    channel_id: serenity::ChannelId,
    user_id: serenity::UserId,
    session_id: String,
    label: String,
    wait: Duration,
) {
    tokio::time::sleep(wait).await;

    let msg = format!(
        "<@{user_id}> `{label}` (`{session_id}`) is now bookable \u{2014} the booking window just \
         opened. Book it in the app.",
    );
    if let Err(e) = channel_id.say(&http, msg).await {
        tracing::warn!("Failed to post booking-window alert: {e}");
    }
}

fn format_session(s: &Session) -> String {
    let date = s.date.as_deref().and_then(|d| d.get(..10)).unwrap_or("?");
    format!(
        "{} | {} | {}",
        s.name.as_deref().unwrap_or(&s.id),
        date,
        s.time.as_deref().unwrap_or("?"),
    )
}

fn format_booking(b: &Booking) -> String {
    let s = b.session.first();
    let date = s
        .and_then(|s| s.date.as_deref())
        .and_then(|d| d.get(..10))
        .unwrap_or("?");
    format!(
        "{} | {} | {}",
        s.and_then(|s| s.name.as_deref()).unwrap_or(&b.session_id),
        date,
        s.and_then(|s| s.time.as_deref()).unwrap_or("?"),
    )
}

/// Send `lines` as one or more messages, splitting at Discord's 2000-character limit.
/// After `ctx.defer()`, the first call fills the deferred reply slot; subsequent calls
/// produce follow-up messages automatically.
async fn send_chunked(ctx: Context<'_>, lines: &[String]) -> Result<(), Error> {
    const LIMIT: usize = 1900; // stay safely under the 2000-char hard cap

    let mut chunk = String::new();
    for line in lines {
        let needed = if chunk.is_empty() {
            line.len()
        } else {
            1 + line.len()
        };
        if !chunk.is_empty() && chunk.len() + needed > LIMIT {
            ctx.say(std::mem::take(&mut chunk)).await?;
        }
        if !chunk.is_empty() {
            chunk.push('\n');
        }
        chunk.push_str(line);
    }
    if !chunk.is_empty() {
        ctx.say(chunk).await?;
    }
    Ok(())
}

async fn autocomplete_session(ctx: Context<'_>, partial: &str) -> Vec<AutocompleteChoice> {
    let config = ctx.data().config.clone();
    let partial = partial.to_lowercase();

    let sessions = with_client(config, MonClubClient::list_sessions)
        .await
        .unwrap_or_default();

    sessions
        .into_iter()
        .filter(|s| {
            partial.is_empty()
                || s.name
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&partial)
        })
        .map(|s| AutocompleteChoice::new(format_session(&s), s.id))
        .take(25)
        .collect()
}

async fn autocomplete_booking(ctx: Context<'_>, partial: &str) -> Vec<AutocompleteChoice> {
    let config = ctx.data().config.clone();
    let partial = partial.to_lowercase();

    let bookings = with_client(config, MonClubClient::list_bookings)
        .await
        .unwrap_or_default();

    bookings
        .into_iter()
        .filter(|b| {
            partial.is_empty()
                || b.session
                    .first()
                    .and_then(|s| s.name.as_deref())
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&partial)
        })
        .map(|b| {
            // Encode both IDs in the value: "booking_id:session_id"
            let value = format!("{}:{}", b.id, b.session_id);
            AutocompleteChoice::new(format_booking(&b), value)
        })
        .take(25)
        .collect()
}

fn format_session_detail(d: &SessionDetail) -> Vec<String> {
    d.display_lines()
        .into_iter()
        .map(|line| {
            let mut parts = line.splitn(2, ": ");
            match (parts.next(), parts.next()) {
                (Some(key), Some(val)) => format!("**{key}:** {val}"),
                _ => line,
            }
        })
        .collect()
}

/// Show detailed info about one of your bookings
#[poise::command(slash_command)]
async fn booking(
    ctx: Context<'_>,
    #[description = "Booking to inspect"]
    #[autocomplete = "autocomplete_booking"]
    booking: String,
) -> Result<(), Error> {
    ensure_owner!(ctx);

    ctx.defer().await?;

    // Value format: "booking_id:session_id"
    let (_booking_id, session_id) = booking
        .split_once(':')
        .ok_or("Invalid booking value — use the autocomplete dropdown")?;

    let sid = session_id.to_string();
    let config = ctx.data().config.clone();

    let detail = with_client(config, move |c| c.get_session(&sid)).await?;

    let lines = format_session_detail(&detail);
    send_chunked(ctx, &lines).await
}

/// List upcoming bookings, optionally for other linked users
#[poise::command(slash_command)]
async fn bookings(
    ctx: Context<'_>,
    #[description = "People to list for (mentions, labels, or @everyone); defaults to you"]
    users: Option<String>,
) -> Result<(), Error> {
    ensure_owner!(ctx);

    ctx.defer().await?;

    let targets = match resolve_targets(&ctx, users.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            ctx.say(e).await?;
            return Ok(());
        }
    };

    // With a single target, list the bookings directly; with several, group
    // each account's bookings under a header so it's clear who has what.
    let multi = targets.len() > 1;
    let mut lines: Vec<String> = Vec::new();

    for account in targets {
        let config = ctx.data().config.clone();
        let label = account.label.clone();

        let bookings = with_account(config, account, MonClubClient::list_bookings).await;

        if multi {
            lines.push(format!("**{label}**"));
        }

        match bookings {
            Ok(bookings) if bookings.is_empty() => {
                lines.push(if multi {
                    "- No upcoming bookings.".to_string()
                } else {
                    "No upcoming bookings.".to_string()
                });
            }
            Ok(bookings) => {
                lines.extend(
                    bookings
                        .iter()
                        .map(|b| format!("- {} (`{}`)", format_booking(b), b.id)),
                );
            }
            Err(e) => lines.push(format!("- Failed to list bookings: {e}")),
        }
    }

    send_chunked(ctx, &lines).await
}

/// List available sessions
#[poise::command(slash_command)]
async fn list(
    ctx: Context<'_>,
    #[description = "Maximum number of sessions to show, ordered by date"]
    #[min = 1]
    limit: Option<u64>,
) -> Result<(), Error> {
    ensure_owner!(ctx);

    ctx.defer().await?;

    let config = ctx.data().config.clone();

    let mut sessions = with_client(config, MonClubClient::list_sessions).await?;

    if sessions.is_empty() {
        ctx.say("No sessions available.").await?;
        return Ok(());
    }

    sessions.sort_by(|a, b| a.date.cmp(&b.date));

    let iter = sessions.iter();
    let lines: Vec<String> = match limit {
        Some(n) => iter
            .take(usize::try_from(n).unwrap_or(usize::MAX))
            .map(|s| format!("- {} (`{}`)", format_session(s), s.id))
            .collect(),
        None => iter
            .map(|s| format!("- {} (`{}`)", format_session(s), s.id))
            .collect(),
    };

    send_chunked(ctx, &lines).await
}

/// Book `session_id` for a group of accounts atomically: if any account fails,
/// the bookings that already succeeded are cancelled (rolled back), so the group
/// is all-or-nothing. Used when booking for more than one person (e.g.
/// `@everyone`). No background retry here — a slot that isn't open yet counts as
/// a failure for the group (use `/prebook` to schedule instead).
async fn book_group_atomic(
    ctx: &Context<'_>,
    session_id: &str,
    targets: &[Account],
) -> Result<(), Error> {
    let mut booked: Vec<Account> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for account in targets {
        let config = ctx.data().config.clone();
        let sid = session_id.to_string();
        let result = with_account(config, account.clone(), move |c| {
            Ok(c.book_session(&Session::from_id(sid)))
        })
        .await;

        match result {
            Ok(Ok(_)) => booked.push(account.clone()),
            Ok(Err(BookError::SlotNotOpen(_))) => {
                failures.push(format!("{} (slot not open yet)", account.label));
            }
            Ok(Err(e)) => failures.push(format!("{} ({e})", account.label)),
            Err(e) => failures.push(format!("{} ({e})", account.label)),
        }
    }

    if failures.is_empty() {
        let labels: Vec<&str> = booked.iter().map(|a| a.label.as_str()).collect();
        ctx.say(format!("Booked `{session_id}` for: {}", labels.join(", ")))
            .await?;
        return Ok(());
    }

    // At least one failed: roll back everyone who was booked in this group.
    let mut rolled_back: Vec<String> = Vec::new();
    let mut rollback_errors: Vec<String> = Vec::new();

    for account in &booked {
        let config = ctx.data().config.clone();
        let sid = session_id.to_string();
        let result = with_account(config, account.clone(), move |c| {
            c.cancel_session_booking(&sid)
        })
        .await;

        match result {
            // Cancelled, or nothing to cancel: either way it is rolled back.
            Ok(_) => rolled_back.push(account.label.clone()),
            Err(e) => rollback_errors.push(format!("{} ({e})", account.label)),
        }
    }

    let mut lines = vec![format!(
        "Booking failed for: {}. Rolled back the whole group.",
        failures.join(", ")
    )];
    if !rolled_back.is_empty() {
        lines.push(format!("Cancelled: {}", rolled_back.join(", ")));
    }
    if !rollback_errors.is_empty() {
        lines.push(format!(
            "WARNING: rollback failed for: {} — please cancel manually.",
            rollback_errors.join(", ")
        ));
    }

    ctx.say(lines.join("\n")).await?;
    Ok(())
}

/// Book a session, optionally for other linked users
#[poise::command(slash_command)]
async fn book(
    ctx: Context<'_>,
    #[description = "Session to book"]
    #[autocomplete = "autocomplete_session"]
    session_id: String,
    #[description = "People to book for (mentions, labels, or @everyone); defaults to you"]
    users: Option<String>,
) -> Result<(), Error> {
    ensure_owner!(ctx);

    ctx.defer().await?;

    let targets = match resolve_targets(&ctx, users.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            ctx.say(e).await?;
            return Ok(());
        }
    };

    // Booking for several people is atomic (all-or-nothing with rollback). A
    // single target keeps the immediate + background-retry behaviour below.
    if targets.len() > 1 {
        return book_group_atomic(&ctx, &session_id, &targets).await;
    }

    let mut booked: Vec<String> = Vec::new();
    let mut retrying: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    for account in targets {
        let config = ctx.data().config.clone();
        let label = account.label.clone();
        let sid = session_id.clone();

        // Handle authenticate errors and BookError separately.
        let book_result = with_account(config, account.clone(), move |c| {
            Ok(c.book_session(&Session::from_id(sid)))
        })
        .await;

        match book_result {
            Ok(Ok(_)) => booked.push(label),
            Ok(Err(BookError::SlotNotOpen(_))) => {
                let retry_duration = ctx.data().config.retry_duration;
                let retry_interval = ctx.data().config.retry_interval;
                let http = ctx.serenity_context().http.clone();
                let channel_id = ctx.channel_id();
                let config = ctx.data().config.clone();

                tokio::spawn(retry_book(
                    http,
                    channel_id,
                    config,
                    account,
                    session_id.clone(),
                    retry_duration,
                    retry_interval,
                ));
                retrying.push(label);
            }
            // A rejection (e.g. no credits) won't be fixed by retrying, so
            // report it with its message rather than scheduling a retry.
            Ok(Err(e @ BookError::Rejected { .. })) => failed.push(format!("{label} ({e})")),
            Ok(Err(BookError::Request(e))) => failed.push(format!("{label} ({e})")),
            Err(e) => failed.push(format!("{label} ({e})")),
        }
    }

    let mut lines: Vec<String> = Vec::new();
    if !booked.is_empty() {
        lines.push(format!("Booked `{session_id}` for: {}", booked.join(", ")));
    }
    if !retrying.is_empty() {
        let retry_duration = ctx.data().config.retry_duration;
        let retry_interval = ctx.data().config.retry_interval;
        lines.push(format!(
            "Slot not open yet for: {}. Retrying every {retry_interval}s for up to {retry_duration}s...",
            retrying.join(", ")
        ));
    }
    if !failed.is_empty() {
        lines.push(format!("Booking failed for: {}", failed.join(", ")));
    }

    ctx.say(lines.join("\n")).await?;
    Ok(())
}

/// Book a session at a scheduled time, optionally for other linked users
#[poise::command(slash_command)]
async fn prebook(
    ctx: Context<'_>,
    #[description = "Session to book"]
    #[autocomplete = "autocomplete_session"]
    session_id: String,
    #[description = "When to book: HH:MM or YYYY-MM-DD HH:MM (local time)"] when: String,
    #[description = "People to book for (mentions or labels); defaults to you"] users: Option<
        String,
    >,
) -> Result<(), Error> {
    ensure_owner!(ctx);

    let target = match parse_when(&when) {
        Ok(t) => t,
        Err(e) => {
            ctx.say(format!("Invalid time: {e}")).await?;
            return Ok(());
        }
    };

    let now = Local::now();
    if target <= now {
        ctx.say("That time is already in the past.").await?;
        return Ok(());
    }

    let wait = match (target - now).to_std() {
        Ok(d) => d,
        Err(e) => {
            ctx.say(format!("Duration error: {e}")).await?;
            return Ok(());
        }
    };

    let targets = match resolve_targets(&ctx, users.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            ctx.say(e).await?;
            return Ok(());
        }
    };

    let labels: Vec<&str> = targets.iter().map(|a| a.label.as_str()).collect();
    ctx.say(format!(
        "Scheduled: will book `{session_id}` for {} at `{}`. I'll post here when done.",
        labels.join(", "),
        target.format("%Y-%m-%d %H:%M:%S")
    ))
    .await?;

    let retry_duration = ctx.data().config.retry_duration;
    let retry_interval = ctx.data().config.retry_interval;

    for account in targets {
        let http = ctx.serenity_context().http.clone();
        let channel_id = ctx.channel_id();
        let config = ctx.data().config.clone();
        let session_id = session_id.clone();

        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            retry_book(
                http,
                channel_id,
                config,
                account,
                session_id,
                retry_duration,
                retry_interval,
            )
            .await;
        });
    }

    Ok(())
}

/// Cancel a booking, optionally for other linked users
#[poise::command(slash_command)]
async fn cancel(
    ctx: Context<'_>,
    #[description = "Booking to cancel"]
    #[autocomplete = "autocomplete_booking"]
    booking: String,
    #[description = "People to cancel for (mentions or labels); defaults to you"] users: Option<
        String,
    >,
) -> Result<(), Error> {
    ensure_owner!(ctx);

    ctx.defer().await?;

    // Value format: "booking_id:session_id". Each target account may hold its
    // own booking for this session, so we cancel by session id per account
    // rather than reusing the caller's booking id.
    let (_booking_id, session_id) = booking
        .split_once(':')
        .ok_or("Invalid booking value — use the autocomplete dropdown")?;
    let session_id = session_id.to_string();

    let targets = match resolve_targets(&ctx, users.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            ctx.say(e).await?;
            return Ok(());
        }
    };

    let mut cancelled: Vec<String> = Vec::new();
    let mut not_found: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    for account in targets {
        let config = ctx.data().config.clone();
        let label = account.label.clone();
        let sid = session_id.clone();

        let result = with_account(config, account, move |c| c.cancel_session_booking(&sid)).await;

        match result {
            Ok(Some(_)) => cancelled.push(label),
            Ok(None) => not_found.push(label),
            Err(e) => failed.push(format!("{label} ({e})")),
        }
    }

    let mut lines: Vec<String> = Vec::new();
    if !cancelled.is_empty() {
        lines.push(format!("Cancelled for: {}", cancelled.join(", ")));
    }
    if !not_found.is_empty() {
        lines.push(format!("No booking found for: {}", not_found.join(", ")));
    }
    if !failed.is_empty() {
        lines.push(format!("Cancellation failed for: {}", failed.join(", ")));
    }
    if lines.is_empty() {
        lines.push("Nothing to cancel.".to_string());
    }

    ctx.say(lines.join("\n")).await?;
    Ok(())
}

fn format_comparison(c: &SessionComparison) -> Vec<String> {
    c.display_lines()
        .into_iter()
        .map(|line| {
            // Bold "Session A:", "Session B:", "In both (...):", etc.
            let mut parts = line.splitn(2, ": ");
            match (parts.next(), parts.next()) {
                (Some(key), Some(val)) if !val.is_empty() => format!("**{key}:** {val}"),
                _ => line,
            }
        })
        .collect()
}

/// Compare participants between two sessions
#[poise::command(slash_command)]
async fn compare(
    ctx: Context<'_>,
    #[description = "First session"]
    #[autocomplete = "autocomplete_session"]
    session_a: String,
    #[description = "Second session"]
    #[autocomplete = "autocomplete_session"]
    session_b: String,
) -> Result<(), Error> {
    ensure_owner!(ctx);

    ctx.defer().await?;

    let config = ctx.data().config.clone();

    let comparison =
        with_client(config, move |c| c.compare_sessions(&session_a, &session_b)).await?;

    let lines = format_comparison(&comparison);
    send_chunked(ctx, &lines).await
}

/// Get pinged here when a session becomes bookable (crosses its booking window)
#[poise::command(slash_command)]
async fn notify(
    ctx: Context<'_>,
    #[description = "Session to watch"]
    #[autocomplete = "autocomplete_session"]
    session_id: String,
) -> Result<(), Error> {
    ensure_owner!(ctx);

    ctx.defer().await?;

    let config = ctx.data().config.clone();
    let sid = session_id.clone();
    let detail = with_client(config, move |c| c.get_session(&sid)).await?;

    let label = detail.name.clone().unwrap_or_else(|| session_id.clone());

    let Some(start) = detail.date.as_deref().and_then(parse_session_start) else {
        ctx.say(format!(
            "Couldn't read the start time for `{label}`, so I can't work out its booking window."
        ))
        .await?;
        return Ok(());
    };

    let window = chrono::Duration::hours(ctx.data().config.booking_window_hours);
    let open_at = start - window;
    let now = Local::now();

    // Already inside the window (or the session is in the past): nothing to wait for.
    let Ok(wait) = (open_at - now).to_std() else {
        ctx.say(format!(
            "`{label}` is already bookable \u{2014} you can book it in the app now."
        ))
        .await?;
        return Ok(());
    };

    ctx.say(format!(
        "Watching `{label}`. I'll ping you here when it opens for booking on `{}` ({}h before the session starts).",
        open_at.format("%Y-%m-%d %H:%M"),
        ctx.data().config.booking_window_hours,
    ))
    .await?;

    tokio::spawn(alert_when_bookable(
        ctx.serenity_context().http.clone(),
        ctx.channel_id(),
        ctx.author().id,
        session_id,
        label,
        wait,
    ));

    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    // Load .env before logging so RUST_LOG set there is picked up.
    dotenvy::dotenv().ok();

    // Keep the guard alive for the entire process — dropping it flushes the log writer.
    let _guard = logging::init("monclub-discord", true);

    let config = Config::from_env();

    let token = config
        .discord_token
        .clone()
        .expect("DISCORD_TOKEN must be set to run the Discord bot");

    let owner_id = config.discord_owner_id.map(serenity::UserId::new);

    let intents = serenity::GatewayIntents::non_privileged();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                bookings(),
                list(),
                book(),
                prebook(),
                cancel(),
                booking(),
                compare(),
                notify(),
            ],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                info!("Discord bot ready. Commands registered globally.");

                // Spawn the new-session watcher when a target channel is set.
                if let Some(channel_id) = config.new_sessions_channel_id {
                    let http = ctx.http.clone();
                    let channel = serenity::ChannelId::new(channel_id);
                    let poll_interval = config.new_sessions_poll_interval;
                    let watcher_config = config.clone();
                    info!(
                        channel_id,
                        poll_interval, "Starting new-session watcher"
                    );
                    tokio::spawn(watch_new_sessions(
                        http,
                        channel,
                        watcher_config,
                        poll_interval,
                    ));
                }

                Ok(Data { config, owner_id })
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .expect("Failed to create Discord client");

    client.start().await.expect("Discord client error");
}

#[cfg(test)]
mod tests {
    use super::{is_everyone, parse_mention, parse_session_start};

    #[test]
    fn parses_plain_and_nickname_mentions() {
        assert_eq!(
            parse_mention("<@123456789012345678>"),
            Some(123_456_789_012_345_678)
        );
        assert_eq!(parse_mention("<@!123>"), Some(123));
    }

    #[test]
    fn rejects_non_mentions() {
        assert_eq!(parse_mention("tom"), None);
        assert_eq!(parse_mention("<@abc>"), None);
        assert_eq!(parse_mention("123"), None); // raw ids are handled separately
    }

    #[test]
    fn recognises_everyone_tokens() {
        assert!(is_everyone("@everyone"));
        assert!(is_everyone("everyone"));
        assert!(is_everyone("all"));
        assert!(is_everyone("ALL"));
        assert!(!is_everyone("tom"));
        assert!(!is_everyone("@tom"));
    }

    #[test]
    fn computes_booking_window_open_time() {
        use chrono::{Duration, SecondsFormat, Utc};

        // The `date` field is the session's start instant in UTC.
        let start = parse_session_start("2026-03-22T08:00:00.000Z").expect("parses");
        // 144h before the start is 6 days earlier, at the same instant.
        let open = (start - Duration::hours(144)).with_timezone(&Utc);
        assert_eq!(
            open.to_rfc3339_opts(SecondsFormat::Secs, true),
            "2026-03-16T08:00:00Z"
        );
    }

    #[test]
    fn rejects_unparseable_session_date() {
        assert!(parse_session_start("not-a-date").is_none());
        assert!(parse_session_start("2026-03-22").is_none());
    }
}
