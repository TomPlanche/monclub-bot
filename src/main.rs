use clap::{Parser, Subcommand};
use monclub_bot::{client, config, logging};

/// `MonClub` session booking assistant.
///
/// All configuration is read from environment variables (or a .env file).
///
/// Required: EMAIL, PASSWORD, `CUSTOM_ID`, `BASE_URL`.
///
/// Optional: LATITUDE, LONGITUDE, `RETRY_DURATION` (default 300s), `RETRY_INTERVAL` (default 5s).
///
/// When called without a subcommand an interactive menu is shown.
#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Book a session
    Book {
        /// Session ID to book directly, skipping the interactive picker
        session_id: Option<String>,
    },
    /// Schedule a booking to run at a specific time
    Prebook {
        /// Session ID to book directly, skipping the interactive picker
        session_id: Option<String>,
        /// When to book: HH:MM or "YYYY-MM-DD HH:MM" (local time)
        when: Option<String>,
    },
    /// View and manage your upcoming bookings
    Manage,
}

fn main() {
    let cli = Cli::parse();

    // Load .env before logging so RUST_LOG set there is picked up.
    dotenvy::dotenv().ok();

    let _guard = logging::init("monclub-bot", false);

    let config = config::Config::from_env();
    let mut client = client::MonClubClient::new(config);

    let result = if let Some(cmd) = cli.command {
        client.authenticate().and_then(|()| match cmd {
            Command::Book { session_id } => client.run_book(session_id),
            Command::Prebook { session_id, when } => client.run_prebook(session_id, when),
            Command::Manage => client.run_manage_bookings(),
        })
    } else {
        client.run()
    };

    if let Err(e) = result {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
