//! Stage, unstage and discard, for whole files and single hunks.
//!
//! Hunk operations are guarded: the hunk list is recomputed from the same diff
//! builders `file_diff` uses, and the caller's `expected_header` must match the
//! header at `hunk_index`. On any mismatch the call returns `CoreError::Stale`
//! before anything is written.
use crate::diff::{delta_index, files_in_target, staged_git_diff, unstaged_git_diff};
use crate::model::{DiffTarget, FileStatus};
use crate::{CoreError, Result};
use git2::build::CheckoutBuilder;
use git2::{
    ApplyLocation, ApplyOptions, Delta, Diff, ErrorCode, IndexEntry, IndexTime, Patch,
    Repository, Status, Tree,
};
use std::cell::Cell;
use std::path::{Component, Path, PathBuf};

fn invalid(msg: &str) -> CoreError {
    CoreError::Git(git2::Error::from_str(msg))
}

/// Rejects empty, absolute and `..` paths so nothing outside the workdir is touched.
fn check_path(path: &str) -> Result<()> {
    let p = Path::new(path);
    let ok = !path.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)));
    if ok {
        Ok(())
    } else {
        Err(invalid(&format!("Invalid repository path: {path}")))
    }
}

fn workdir_path(repo: &Repository, path: &str) -> Result<PathBuf> {
    let wd = repo.workdir().ok_or_else(|| invalid("Repository has no working directory"))?;
    Ok(wd.join(path))
}

/// HEAD tree, None when HEAD is unborn. Other errors propagate.
fn head_tree_strict(repo: &Repository) -> Result<Option<Tree<'_>>> {
    match repo.head() {
        Ok(h) => Ok(Some(h.peel_to_tree()?)),
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn stage_file(repo: &Repository, path: &str) -> Result<()> {
    check_path(path)?;
    let on_disk = workdir_path(repo, path)?.symlink_metadata().is_ok();
    let mut idx = repo.index()?;
    if on_disk {
        idx.add_path(Path::new(path))?;
    } else {
        idx.remove_path(Path::new(path))?;
    }
    idx.write()?;
    Ok(())
}

/// Old path of a staged rename whose new path is `path`, if any.
fn staged_rename_source(repo: &Repository, path: &str) -> Result<Option<String>> {
    Ok(files_in_target(repo, &DiffTarget::Staged)?
        .into_iter()
        .find(|e| e.path == path && e.status == FileStatus::Renamed)
        .and_then(|e| e.old_path))
}

/// Resets the index entries for `paths` to HEAD (or removes them when HEAD lacks
/// them or is unborn). Same effect as `git reset -- <paths>`, but with literal
/// paths: libgit2's `reset_default` treats paths as glob pathspecs, so a file
/// named `[ab].txt` would also unstage `a.txt`. Workdir is never touched.
fn reset_index_paths(repo: &Repository, paths: &[&str]) -> Result<()> {
    let tree = head_tree_strict(repo)?;
    let mut idx = repo.index()?;
    for p in paths {
        let pp = Path::new(p);
        if let Err(e) = idx.conflict_remove(pp) {
            if e.code() != ErrorCode::NotFound {
                return Err(e.into());
            }
        }
        let head_entry = match &tree {
            Some(t) => match t.get_path(pp) {
                Ok(te) if te.kind() != Some(git2::ObjectType::Tree) => Some(te),
                Ok(_) => None,
                Err(e) if e.code() == ErrorCode::NotFound => None,
                Err(e) => return Err(e.into()),
            },
            None => None,
        };
        match head_entry {
            Some(te) => {
                let zero = IndexTime::new(0, 0);
                idx.add(&IndexEntry {
                    ctime: zero,
                    mtime: zero,
                    dev: 0,
                    ino: 0,
                    mode: te.filemode() as u32,
                    uid: 0,
                    gid: 0,
                    file_size: 0,
                    id: te.id(),
                    flags: 0,
                    flags_extended: 0,
                    path: p.as_bytes().to_vec(),
                })?;
            }
            None => idx.remove_path(pp)?,
        }
    }
    idx.write()?;
    Ok(())
}

/// Moves `path` back to its HEAD state in the index. For a staged rename the old
/// path is restored too, so the whole rename is unstaged and the new file is
/// left untracked on disk.
pub fn unstage_file(repo: &Repository, path: &str) -> Result<()> {
    check_path(path)?;
    let old = staged_rename_source(repo, path)?;
    let mut paths = vec![path];
    if let Some(o) = old.as_deref() {
        check_path(o)?;
        paths.push(o);
    }
    reset_index_paths(repo, &paths)
}

/// Throws away unstaged changes to `path`. Untracked files are deleted; tracked
/// files are restored from the index, so staged changes are kept.
pub fn discard_file(repo: &Repository, path: &str) -> Result<()> {
    check_path(path)?;
    let status = repo.status_file(Path::new(path))?;
    if status.contains(Status::CONFLICTED) {
        return Err(invalid("Cannot discard a conflicted file"));
    }
    let in_index = repo.index()?.get_path(Path::new(path), 0).is_some();
    if !in_index {
        if status == Status::WT_NEW {
            std::fs::remove_file(workdir_path(repo, path)?)?;
            return Ok(());
        }
        return Err(CoreError::Stale);
    }
    let mut co = CheckoutBuilder::new();
    co.force().disable_pathspec_match(true).path(path);
    repo.checkout_index(None, Some(&mut co))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HunkSig {
    header: String,
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
}

/// Delta status and hunk signatures of `path` in `diff`, None when absent.
fn hunk_sigs(diff: &Diff<'_>, path: &str) -> Result<Option<(Delta, Vec<HunkSig>)>> {
    let Some(i) = delta_index(diff, path) else {
        return Ok(None);
    };
    let Some(patch) = Patch::from_diff(diff, i)? else {
        return Ok(None);
    };
    let status = patch.delta().status();
    let mut out = Vec::with_capacity(patch.num_hunks());
    for h in 0..patch.num_hunks() {
        let (hd, _) = patch.hunk(h)?;
        out.push(HunkSig {
            header: String::from_utf8_lossy(hd.header()).trim().to_string(),
            old_start: hd.old_start(),
            old_lines: hd.old_lines(),
            new_start: hd.new_start(),
            new_lines: hd.new_lines(),
        });
    }
    Ok(Some((status, out)))
}

/// Checks `expected_header` against the forward diff. Returns the delta status
/// and all forward hunk signatures, or `Stale`.
fn guard(
    forward: &Diff<'_>,
    path: &str,
    hunk_index: usize,
    expected_header: &str,
) -> Result<(Delta, Vec<HunkSig>)> {
    let Some((status, sigs)) = hunk_sigs(forward, path)? else {
        return Err(CoreError::Stale);
    };
    match sigs.get(hunk_index) {
        Some(s) if s.header == expected_header.trim() => Ok((status, sigs)),
        _ => Err(CoreError::Stale),
    }
}

/// Applies only the `hunk_index`-th hunk of `path` from `diff` at `location`.
/// `forward` are the hunks the caller verified; when `diff` is the reversed diff
/// it must be their exact mirror, otherwise the file moved in between and we
/// return `Stale` without writing.
fn apply_one(
    repo: &Repository,
    diff: &Diff<'_>,
    path: &str,
    hunk_index: usize,
    forward: &[HunkSig],
    reversed: bool,
    location: ApplyLocation,
) -> Result<()> {
    let Some((status, sigs)) = hunk_sigs(diff, path)? else {
        return Err(CoreError::Stale);
    };
    // libgit2 applies Added and Deleted deltas as a whole and never calls the hunk
    // callback for them. Such a delta has exactly one hunk, so applying it whole is
    // the same as applying that hunk; anything else is refused.
    let whole_file = matches!(status, Delta::Added | Delta::Deleted);
    if whole_file && (sigs.len() != 1 || hunk_index != 0) {
        return Err(CoreError::Stale);
    }
    let mirrors = sigs.len() == forward.len()
        && sigs.iter().zip(forward).all(|(a, f)| {
            if reversed {
                (a.old_start, a.old_lines, a.new_start, a.new_lines)
                    == (f.new_start, f.new_lines, f.old_start, f.old_lines)
            } else {
                a == f
            }
        });
    if !mirrors {
        return Err(CoreError::Stale);
    }
    let seen = Cell::new(0usize);
    let applied = Cell::new(0usize);
    let mut opts = ApplyOptions::new();
    opts.delta_callback(|d| {
        d.is_some_and(|d| {
            [d.new_file().path(), d.old_file().path()]
                .into_iter()
                .flatten()
                .any(|p| p.to_str() == Some(path))
        })
    });
    opts.hunk_callback(|_| {
        let n = seen.get();
        seen.set(n + 1);
        let take = n == hunk_index;
        if take {
            applied.set(applied.get() + 1);
        }
        take
    });
    repo.apply(diff, location, Some(&mut opts))?;
    if !whole_file {
        debug_assert_eq!(applied.get(), 1, "exactly one hunk must be applied");
    }
    Ok(())
}

pub fn stage_hunk(
    repo: &Repository,
    path: &str,
    hunk_index: usize,
    expected_header: &str,
) -> Result<()> {
    check_path(path)?;
    let diff = unstaged_git_diff(repo, path, false)?;
    let (status, sigs) = guard(&diff, path, hunk_index, expected_header)?;
    if status == Delta::Untracked {
        return stage_file(repo, path);
    }
    apply_one(repo, &diff, path, hunk_index, &sigs, false, ApplyLocation::Index)
}

pub fn unstage_hunk(
    repo: &Repository,
    path: &str,
    hunk_index: usize,
    expected_header: &str,
) -> Result<()> {
    check_path(path)?;
    let forward = staged_git_diff(repo, path, false)?;
    let (_, sigs) = guard(&forward, path, hunk_index, expected_header)?;
    let reversed = staged_git_diff(repo, path, true)?;
    apply_one(repo, &reversed, path, hunk_index, &sigs, true, ApplyLocation::Index)
}

pub fn discard_hunk(
    repo: &Repository,
    path: &str,
    hunk_index: usize,
    expected_header: &str,
) -> Result<()> {
    check_path(path)?;
    let forward = unstaged_git_diff(repo, path, false)?;
    let (status, sigs) = guard(&forward, path, hunk_index, expected_header)?;
    if status == Delta::Untracked {
        return discard_file(repo, path);
    }
    let reversed = unstaged_git_diff(repo, path, true)?;
    apply_one(repo, &reversed, path, hunk_index, &sigs, true, ApplyLocation::WorkDir)
}
