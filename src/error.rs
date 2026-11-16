use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{0}")]
    Git(#[from] git2::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("Not a Git repository: {}", .0.display())]
    NotARepo(PathBuf),
    #[error("File changed, refreshed. Try again.")]
    Stale,
    #[error("Your local changes would be overwritten. Commit or discard them first.")]
    CheckoutConflict,
    #[error("{0}")]
    Flow(String),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, CoreError>;
