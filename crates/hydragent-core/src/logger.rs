use tracing_subscriber::{fmt, prelude::*, Registry, EnvFilter};

/// Initializes the logging system based on the format requested.
/// - "json": Outputs structured JSON logs for log aggregators in production.
/// - "terminal" or anything else: Outputs pretty color-coded ANSI terminal logs.
pub fn init_logger(format: &str, level: &str) {
    // Determine target log level
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    if format.eq_ignore_ascii_case("json") {
        let subscriber = Registry::default()
            .with(filter)
            .with(fmt::layer().json());
        tracing::subscriber::set_global_default(subscriber)
            .expect("Failed to set logging subscriber");
    } else {
        let subscriber = Registry::default()
            .with(filter)
            .with(fmt::layer().with_ansi(true));
        tracing::subscriber::set_global_default(subscriber)
            .expect("Failed to set logging subscriber");
    }
}
