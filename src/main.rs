mod client;
mod config;

use simplelog::{Config, LevelFilter, WriteLogger};
use std::fs::File;

fn main() {
    WriteLogger::init(
        LevelFilter::Debug,
        Config::default(),
        File::create("monclub-bot.log").expect("failed to create log file"),
    )
    .expect("failed to init logger");

    dotenvy::dotenv().ok();

    let config = config::Config::from_env();
    if let Err(e) = client::MonClubClient::new(config).run() {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
