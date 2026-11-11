use crate::error::{CoreError, Result};
use git2::Repository;
use std::path::Path;

pub fn open_repo(path: &Path) -> Result<Repository> {
    match Repository::open(path) {
        Ok(r) if r.is_bare() => Err(CoreError::NotARepo(path.to_path_buf())),
        Ok(r) => Ok(r),
        Err(e) if e.code() == git2::ErrorCode::NotFound => {
            Err(CoreError::NotARepo(path.to_path_buf()))
        }
        Err(e) => Err(e.into()),
    }
}

pub fn repo_name(repo: &Repository) -> String {
    repo.workdir()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
