use monclub_bot::{client, config, logging};

fn main() {
    // Load .env before logging so RUST_LOG set there is picked up.
    dotenvy::dotenv().ok();

    let _guard = logging::init("monclub-bot", false);

    let config = config::Config::from_env();
    if let Err(e) = client::MonClubClient::new(config).run() {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
