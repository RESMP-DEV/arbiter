#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Timeout(String),
    #[error("{0}")]
    NotOwner(String),
    #[error("{0}")]
    Failed(String),
    #[error("{0}")]
    Env(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{context}: {source}")]
    Config {
        context: String,
        source: toml::de::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => 2,
            Error::Conflict(_) => 3,
            Error::Timeout(_) => 4,
            Error::NotOwner(_) | Error::Failed(_) => 5,
            Error::Env(_) | Error::Config { .. } => 6,
            Error::Io(_) | Error::Db(_) | Error::Json(_) => 1,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Error::Usage(_) => "usage",
            Error::Conflict(_) => "conflict",
            Error::Timeout(_) => "timeout",
            Error::NotOwner(_) => "not_owner",
            Error::Failed(_) => "failed",
            Error::Env(_) => "environment",
            Error::Config { .. } => "config",
            Error::Io(_) => "io",
            Error::Db(_) => "database",
            Error::Json(_) => "json",
        }
    }
}
