use lazydocker_rs::{app::App, config::AppConfig, logging};

#[tokio::main]
async fn main() {
    logging::init();

    if let Err(err) = run().await {
        tracing::error!(%err, "application exited with an error");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), lazydocker_rs::errors::AppError> {
    let config = AppConfig::load()?;
    let _final_state = App::new(config)?.run().await?;
    Ok(())
}
