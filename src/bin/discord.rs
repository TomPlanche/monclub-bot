use std::sync::Arc;
use std::time::Duration;

use chrono::Local;
use monclub_bot::client::{
    BookError, Booking, MonClubClient, Session, SessionComparison, SessionDetail, parse_when,
};
use monclub_bot::config::Config;
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

/// Retry booking `session_id` in the background until the slot opens or the
/// deadline passes, posting the outcome to `channel_id`.
async fn retry_book(
    http: Arc<serenity::Http>,
    channel_id: serenity::ChannelId,
    config: Config,
    session_id: String,
    retry_duration: u64,
    retry_interval: u64,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(retry_duration);

    loop {
        let sid = session_id.clone();
        let result = with_client(config.clone(), move |c| {
            Ok(c.book_session(&Session::from_id(sid)))
        })
        .await;

        match result {
            Ok(Ok(_)) => {
                let _ = channel_id
                    .say(&http, format!("Booked: `{session_id}`"))
                    .await;
                return;
            }
            Ok(Err(BookError::SlotNotOpen(_))) => {
                if tokio::time::Instant::now() >= deadline {
                    let _ = channel_id.say(&http, "Booking window expired.").await;
                    return;
                }
                tokio::time::sleep(Duration::from_secs(retry_interval)).await;
            }
            _ => {
                let _ = channel_id
                    .say(&http, "Booking failed with an unexpected error.")
                    .await;
                return;
            }
        }
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

/// List your upcoming bookings
#[poise::command(slash_command)]
async fn bookings(ctx: Context<'_>) -> Result<(), Error> {
    ensure_owner!(ctx);

    ctx.defer().await?;

    let config = ctx.data().config.clone();

    let bookings = with_client(config, MonClubClient::list_bookings).await?;

    if bookings.is_empty() {
        ctx.say("No upcoming bookings.").await?;
        return Ok(());
    }

    let lines: Vec<String> = bookings
        .iter()
        .map(|b| format!("- {}", format_booking(b)))
        .collect();
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

/// Book a session
#[poise::command(slash_command)]
async fn book(
    ctx: Context<'_>,
    #[description = "Session to book"]
    #[autocomplete = "autocomplete_session"]
    session_id: String,
) -> Result<(), Error> {
    ensure_owner!(ctx);

    ctx.defer().await?;

    let config = ctx.data().config.clone();
    let sid = session_id.clone();

    // Handle authenticate errors and BookError separately.
    let book_result =
        with_client(config, move |c| Ok(c.book_session(&Session::from_id(sid)))).await?;

    match book_result {
        Ok(_) => {
            ctx.say(format!("Booked: `{session_id}`")).await?;
        }
        Err(BookError::SlotNotOpen(_)) => {
            let retry_duration = ctx.data().config.retry_duration;
            let retry_interval = ctx.data().config.retry_interval;

            ctx.say(format!(
                "Slot not open yet. Retrying every {retry_interval}s for up to {retry_duration}s..."
            ))
            .await?;

            let http = ctx.serenity_context().http.clone();
            let channel_id = ctx.channel_id();
            let config = ctx.data().config.clone();

            tokio::spawn(retry_book(
                http,
                channel_id,
                config,
                session_id,
                retry_duration,
                retry_interval,
            ));
        }
        Err(BookError::Request(e)) => {
            ctx.say(format!("Booking failed: {e}")).await?;
        }
    }

    Ok(())
}

/// Book a session at a scheduled time
#[poise::command(slash_command)]
async fn prebook(
    ctx: Context<'_>,
    #[description = "Session to book"]
    #[autocomplete = "autocomplete_session"]
    session_id: String,
    #[description = "When to book: HH:MM or YYYY-MM-DD HH:MM (local time)"] when: String,
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

    ctx.say(format!(
        "Scheduled: will book `{session_id}` at `{}`. I'll post here when done.",
        target.format("%Y-%m-%d %H:%M:%S")
    ))
    .await?;

    let http = ctx.serenity_context().http.clone();
    let channel_id = ctx.channel_id();
    let config = ctx.data().config.clone();
    let retry_duration = config.retry_duration;
    let retry_interval = config.retry_interval;

    tokio::spawn(async move {
        tokio::time::sleep(wait).await;
        retry_book(
            http,
            channel_id,
            config,
            session_id,
            retry_duration,
            retry_interval,
        )
        .await;
    });

    Ok(())
}

/// Cancel a booking
#[poise::command(slash_command)]
async fn cancel(
    ctx: Context<'_>,
    #[description = "Booking to cancel"]
    #[autocomplete = "autocomplete_booking"]
    booking: String,
) -> Result<(), Error> {
    ensure_owner!(ctx);

    ctx.defer().await?;

    // Value format: "booking_id:session_id"
    let (booking_id, session_id) = booking
        .split_once(':')
        .ok_or("Invalid booking value — use the autocomplete dropdown")?;

    let b = Booking {
        id: booking_id.to_string(),
        session_id: session_id.to_string(),
        session: vec![],
    };

    let config = ctx.data().config.clone();

    with_client(config, move |c| c.cancel_booking(&b).map(|_| ())).await?;

    ctx.say(format!("Cancelled booking `{booking_id}`."))
        .await?;
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
            ],
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                info!("Discord bot ready. Commands registered globally.");
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
