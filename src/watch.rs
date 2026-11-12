//! Debounced filesystem watcher for a repository working tree and its Git dirs.
use crate::Result;
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer_opt, DebounceEventResult, Debouncer, NoCache};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// Whether `inner` (relative to a Git dir) is state that changes what we show:
/// the index, HEAD, packed-refs or anything under refs/.
fn is_git_state(inner: &Path) -> bool {
    let mut parts = inner.components();
    let first = parts.next().and_then(|c| c.as_os_str().to_str());
    match first {
        Some("index" | "HEAD" | "packed-refs") => parts.next().is_none(),
        Some("refs") => true,
        _ => false,
    }
}

/// Whether a changed path should trigger a refresh, for a working tree at
/// `root` whose Git dirs are `git_dirs` (the gitdir and, for a linked worktree,
/// the common dir; either may live outside `root`). Inside a Git dir only
/// index, HEAD, packed-refs and refs/** count. `is_ignored` receives the path
/// relative to `root`.
pub fn is_relevant_in(
    root: &Path,
    git_dirs: &[PathBuf],
    path: &Path,
    is_ignored: &dyn Fn(&Path) -> bool,
) -> bool {
    let mut in_git_dir = false;
    for g in git_dirs {
        if let Ok(inner) = path.strip_prefix(g) {
            if is_git_state(inner) {
                return true;
            }
            in_git_dir = true;
        }
    }
    if in_git_dir {
        return false;
    }
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    // `.git` itself (a file in a linked worktree) or anything below it that
    // is not one of the listed Git dirs.
    if rel.components().next() == Some(Component::Normal(".git".as_ref())) {
        return false;
    }
    !is_ignored(rel)
}

/// `is_relevant_in` for an ordinary repo whose Git dir is `root/.git`.
pub fn is_relevant(root: &Path, path: &Path, is_ignored: &dyn Fn(&Path) -> bool) -> bool {
    is_relevant_in(root, &[root.join(".git")], path, is_ignored)
}

/// Watches a repo and calls `on_change` once per relevant debounced batch.
/// Dropping the watcher stops it.
pub struct RepoWatcher {
    _debouncer: Debouncer<notify::RecommendedWatcher, NoCache>,
}

impl RepoWatcher {
    pub fn start(root: PathBuf, on_change: impl Fn() + Send + 'static) -> Result<RepoWatcher> {
        let root = root.canonicalize()?;
        let repo = git2::Repository::open(&root).ok();
        // Gitdir and common dir; in a linked worktree (or any repo whose .git
        // is a file) they live outside the working tree.
        let mut git_dirs: Vec<PathBuf> = vec![root.join(".git")];
        if let Some(r) = &repo {
            for d in [r.path(), r.commondir()] {
                if let Ok(c) = d.canonicalize() {
                    if !git_dirs.contains(&c) {
                        git_dirs.push(c);
                    }
                }
            }
        }
        // Extra watch roots: Git dirs outside `root`, minus ones nested in another.
        let outside: Vec<PathBuf> = git_dirs
            .iter()
            .filter(|d| !d.starts_with(&root))
            .cloned()
            .collect();
        let extra: Vec<PathBuf> = outside
            .iter()
            .filter(|d| !outside.iter().any(|o| o != *d && d.starts_with(o)))
            .cloned()
            .collect();

        let watch_root = root.clone();
        // NoCache: the default file-id cache walks the whole tree (including
        // ignored node_modules/target) before watching starts, which took
        // seconds on large repos. It only pairs rename events, which we never
        // use: any relevant change just triggers a refresh.
        let mut debouncer = new_debouncer_opt::<_, notify::RecommendedWatcher, NoCache>(
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
                    .any(|p| is_relevant_in(&root, &git_dirs, p, &ignored));
                if relevant {
                    on_change();
                }
            },
            NoCache::new(),
            notify::Config::default(),
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        for dir in std::iter::once(&watch_root).chain(&extra) {
            debouncer
                .watch(dir, RecursiveMode::Recursive)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        Ok(RepoWatcher { _debouncer: debouncer })
    }
}
