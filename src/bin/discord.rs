use std::time::Duration;

use monclub_bot::client::{BookError, Booking, MonClubClient, Session, SessionDetail};
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

    let sessions = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Session>> {
        let mut client = MonClubClient::new(config);
        client.authenticate()?;
        client.list_sessions()
    })
    .await
    .ok()
    .and_then(Result::ok)
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

    let bookings = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Booking>> {
        let mut client = MonClubClient::new(config);
        client.authenticate()?;
        client.list_bookings()
    })
    .await
    .ok()
    .and_then(Result::ok)
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
    if !is_owner(&ctx) {
        ctx.say("Unauthorized.").await?;
        return Ok(());
    }

    ctx.defer().await?;

    // Value format: "booking_id:session_id"
    let (_booking_id, session_id) = booking
        .split_once(':')
        .ok_or("Invalid booking value — use the autocomplete dropdown")?;

    let sid = session_id.to_string();
    let config = ctx.data().config.clone();

    let detail = tokio::task::spawn_blocking(move || -> anyhow::Result<SessionDetail> {
        let mut client = MonClubClient::new(config);
        client.authenticate()?;
        client.get_session(&sid)
    })
    .await??;

    let lines = format_session_detail(&detail);
    send_chunked(ctx, &lines).await
}

/// List your upcoming bookings
#[poise::command(slash_command)]
async fn bookings(ctx: Context<'_>) -> Result<(), Error> {
    if !is_owner(&ctx) {
        ctx.say("Unauthorized.").await?;
        return Ok(());
    }

    ctx.defer().await?;

    let config = ctx.data().config.clone();

    let bookings = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Booking>> {
        let mut client = MonClubClient::new(config);
        client.authenticate()?;
        client.list_bookings()
    })
    .await??;

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
    if !is_owner(&ctx) {
        ctx.say("Unauthorized.").await?;
        return Ok(());
    }

    ctx.defer().await?;

    let config = ctx.data().config.clone();

    let mut sessions = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Session>> {
        let mut client = MonClubClient::new(config);
        client.authenticate()?;
        client.list_sessions()
    })
    .await??;

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
    if !is_owner(&ctx) {
        ctx.say("Unauthorized.").await?;
        return Ok(());
    }

    ctx.defer().await?;

    let config = ctx.data().config.clone();
    let sid = session_id.clone();

    // Wrap the booking call so authenticate errors and BookError are handled separately
    let book_result = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Result<serde_json::Value, BookError>> {
            let mut client = MonClubClient::new(config);
            client.authenticate()?;
            Ok(client.book_session(&Session {
                id: sid,
                name: None,
                date: None,
                time: None,
            }))
        },
    )
    .await??;

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

            tokio::spawn(async move {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(retry_duration);

                loop {
                    tokio::time::sleep(Duration::from_secs(retry_interval)).await;

                    if tokio::time::Instant::now() >= deadline {
                        let _ = channel_id.say(&http, "Booking window expired.").await;
                        return;
                    }

                    let config = config.clone();
                    let sid = session_id.clone();

                    let result = tokio::task::spawn_blocking(
                        move || -> anyhow::Result<Result<serde_json::Value, BookError>> {
                            let mut client = MonClubClient::new(config);
                            client.authenticate()?;
                            Ok(client.book_session(&Session {
                                id: sid,
                                name: None,
                                date: None,
                                time: None,
                            }))
                        },
                    )
                    .await;

                    match result {
                        Ok(Ok(Ok(_))) => {
                            let _ = channel_id
                                .say(&http, format!("Booked: `{session_id}`"))
                                .await;
                            return;
                        }
                        Ok(Ok(Err(BookError::SlotNotOpen(_)))) => {
                            // keep retrying
                        }
                        _ => {
                            let _ = channel_id
                                .say(&http, "Booking failed with an unexpected error.")
                                .await;
                            return;
                        }
                    }
                }
            });
        }
        Err(BookError::Request(e)) => {
            ctx.say(format!("Booking failed: {e}")).await?;
        }
    }

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
    if !is_owner(&ctx) {
        ctx.say("Unauthorized.").await?;
        return Ok(());
    }

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

    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut client = MonClubClient::new(config);
        client.authenticate()?;
        client.cancel_booking(&b)?;
        Ok(())
    })
    .await??;

    ctx.say(format!("Cancelled booking `{booking_id}`."))
        .await?;
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
            commands: vec![bookings(), list(), book(), cancel(), booking()],
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
