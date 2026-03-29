use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Initialise logging for a service.
///
/// - Writes daily-rotated files to `logs/<service_name>.YYYY-MM-DD`, keeping up to 30 days.
/// - When `with_stdout` is `true`, also mirrors logs to stdout (useful for systemd / Docker).
/// - Log level is read from `RUST_LOG`; defaults to `info` when unset or invalid.
///   Call this **after** `dotenvy::dotenv()` so `RUST_LOG` set in `.env` is respected.
///
/// The returned [`WorkerGuard`] must be kept alive for the duration of the process.
/// Dropping it flushes and closes the background writer thread.
pub fn init(service_name: &str, with_stdout: bool) -> WorkerGuard {
    std::fs::create_dir_all("logs").expect("failed to create logs/ directory");

    let file_appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(service_name)
        .filename_suffix("log")
        .max_log_files(30)
        .build("logs")
        .expect("failed to initialise rolling file appender");

    let (non_blocking_file, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // File layer: no colour codes, includes file/line for production debugging.
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking_file)
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true);

    // Optional stdout layer: colour codes, omits file/line (already in the file).
    let stdout_layer = with_stdout.then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stdout)
            .with_file(false)
            .with_line_number(false)
    });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .with(stdout_layer)
        .init();

    guard
}
