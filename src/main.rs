use crossterm::{
    cursor::Show,
    event::DisableMouseCapture,
    execute,
    terminal::{LeaveAlternateScreen, disable_raw_mode},
};
use erio::{app::App, config::AppConfig, logging};

#[tokio::main]
async fn main() {
    logging::init();

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort terminal restore so a panic doesn't leave the user
        // stuck in raw mode / alternate screen.
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, Show, DisableMouseCapture);
        original_hook(info);
    }));

    if let Err(err) = run().await {
        tracing::error!(%err, "application exited with an error");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), erio::errors::AppError> {
    let config = AppConfig::load()?;
    let _final_state = App::new(config)?.run().await?;
    Ok(())
}
