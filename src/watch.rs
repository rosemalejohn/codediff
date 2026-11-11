//! Debounced filesystem watcher for a repository working tree.
use crate::Result;
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Whether a changed path should trigger a refresh.
/// `is_ignored` receives the path relative to `root`.
pub fn is_relevant(root: &Path, path: &Path, is_ignored: &dyn Fn(&Path) -> bool) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    if let Ok(inner) = rel.strip_prefix(".git") {
        let mut parts = inner.components();
        let first = parts.next().and_then(|c| c.as_os_str().to_str());
        return match first {
            Some("index" | "HEAD" | "packed-refs") => parts.next().is_none(),
            Some("refs") => true,
            _ => false,
        };
    }
    !is_ignored(rel)
}

/// Watches a repo and calls `on_change` once per relevant debounced batch.
/// Dropping the watcher stops it.
pub struct RepoWatcher {
    _debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
}

impl RepoWatcher {
    pub fn start(root: PathBuf, on_change: impl Fn() + Send + 'static) -> Result<RepoWatcher> {
        let root = root.canonicalize()?;
        let watch_root = root.clone();
        let repo = git2::Repository::open(&root).ok();
        let mut debouncer = new_debouncer(
            Duration::from_millis(150),
            None,
            move |res: DebounceEventResult| {
                let Ok(events) = res else { return };
                let ignored = |rel: &Path| {
                    repo.as_ref()
                        .map(|r| r.is_path_ignored(rel).unwrap_or(false))
                        .unwrap_or(false)
                };
                let relevant = events
                    .iter()
                    .flat_map(|e| e.event.paths.iter())
                    .any(|p| is_relevant(&root, p, &ignored));
                if relevant {
                    on_change();
                }
            },
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        debouncer
            .watch(&watch_root, RecursiveMode::Recursive)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(RepoWatcher { _debouncer: debouncer })
    }
}
