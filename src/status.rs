//! Working tree status listing.
use crate::model::{FileEntry, FileStatus, StatusLists};
use crate::Result;
use git2::{Delta, Repository, StatusOptions};

fn entry(delta: Option<git2::DiffDelta<'_>>) -> Option<FileEntry> {
    let d = delta?;
    let new = d.new_file().path()?.to_string_lossy().into_owned();
    let (status, old_path) = match d.status() {
        Delta::Added => (FileStatus::Added, None),
        Delta::Untracked => (FileStatus::Untracked, None),
        Delta::Deleted => (FileStatus::Deleted, None),
        Delta::Renamed => (
            FileStatus::Renamed,
            d.old_file().path().map(|p| p.to_string_lossy().into_owned()),
        ),
        Delta::Modified | Delta::Typechange | Delta::Copied | Delta::Conflicted => {
            (FileStatus::Modified, None)
        }
        _ => return None,
    };
    Some(FileEntry { path: new, old_path, status })
}

pub fn list_status(repo: &Repository) -> Result<StatusLists> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true)
        .exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    for s in statuses.iter() {
        if let Some(e) = entry(s.head_to_index()) {
            staged.push(e);
        }
        if let Some(e) = entry(s.index_to_workdir()) {
            unstaged.push(e);
        }
    }
    staged.sort_by(|a, b| a.path.cmp(&b.path));
    unstaged.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(StatusLists { staged, unstaged })
}
