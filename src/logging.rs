use std::fs::OpenOptions;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("erio=info,warn"));

    match OpenOptions::new()
        .create(true)
        .append(true)
        .open("erio.log")
    {
        Ok(log_file) => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt::layer().with_writer(move || {
                    log_file
                        .try_clone()
                        .map(|f| Box::new(f) as Box<dyn std::io::Write + Send>)
                        .unwrap_or_else(|_| Box::new(std::io::stderr()))
                }))
                .init();
        }
        Err(err) => {
            eprintln!("Warning: failed to open erio.log ({err}); logging to stderr");
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt::layer().with_writer(std::io::stderr))
                .init();
        }
    }
}
