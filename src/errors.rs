use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("config error: {0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("docker error: {0}")]
    Docker(#[from] crate::docker::DockerError),
    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),
    #[error("event bus is closed")]
    EventBusClosed,
    #[error("task join error: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub type AppResult<T> = Result<T, AppError>;
