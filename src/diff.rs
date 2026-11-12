//! File diffs for every diff target.
use crate::model::{DiffBody, DiffLine, DiffTarget, FileDiff, FileEntry, Hunk, LineKind};
use crate::status::entry_from_delta;
use crate::Result;
use git2::{Diff, DiffFindOptions, DiffOptions, ErrorCode, Patch, Repository, Tree};

const MAX_LINES: usize = 5_000;
const MAX_BLOB_BYTES: u64 = 2 * 1024 * 1024;

fn base_opts(path: Option<&str>, reverse: bool) -> DiffOptions {
    let mut o = DiffOptions::new();
    o.include_untracked(true)
        .show_untracked_content(true)
        .recurse_untracked_dirs(true)
        .reverse(reverse);
    if let Some(p) = path {
        o.pathspec(p).disable_pathspec_match(true);
    }
    o
}

/// Index to workdir, untracked files included with content. Shared with hunk apply.
pub(crate) fn unstaged_git_diff<'r>(
    repo: &'r Repository,
    path: &str,
    reverse: bool,
) -> Result<Diff<'r>> {
    let mut o = base_opts(Some(path), reverse);
    Ok(repo.diff_index_to_workdir(None, Some(&mut o))?)
}

/// HEAD tree, None when HEAD is unborn. Other errors propagate, so a transient
/// HEAD read failure is an error rather than "every file Added".
pub(crate) fn head_tree(repo: &Repository) -> Result<Option<Tree<'_>>> {
    match repo.head() {
        Ok(h) => Ok(Some(h.peel_to_tree()?)),
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// HEAD tree (empty when unborn) to index. Shared with hunk apply.
pub(crate) fn staged_git_diff<'r>(
    repo: &'r Repository,
    path: &str,
    reverse: bool,
) -> Result<Diff<'r>> {
    let tree = head_tree(repo)?;
    let mut o = base_opts(Some(path), reverse);
    Ok(repo.diff_tree_to_index(tree.as_ref(), None, Some(&mut o))?)
}

fn tree_pair<'r>(repo: &'r Repository, t: &DiffTarget) -> Result<(Option<Tree<'r>>, Tree<'r>)> {
    Ok(match t {
        DiffTarget::Commit(oid) => {
            let c = repo.find_commit(*oid)?;
            let parent = if c.parent_count() > 0 { Some(c.parent(0)?.tree()?) } else { None };
            (parent, c.tree()?)
        }
        DiffTarget::Range { base, head } => {
            (Some(repo.find_commit(*base)?.tree()?), repo.find_commit(*head)?.tree()?)
        }
        _ => unreachable!("tree_pair only for commit targets"),
    })
}

fn find_renames(diff: &mut Diff<'_>) -> Result<()> {
    let mut f = DiffFindOptions::new();
    f.renames(true);
    diff.find_similar(Some(&mut f))?;
    Ok(())
}

fn tree_diff<'r>(
    repo: &'r Repository,
    old: Option<&Tree<'r>>,
    new: Option<&Tree<'r>>,
    path: Option<&str>,
    index_as_new: bool,
) -> Result<Diff<'r>> {
    let mut o = DiffOptions::new();
    if let Some(p) = path {
        o.pathspec(p).disable_pathspec_match(true);
    }
    Ok(if index_as_new {
        repo.diff_tree_to_index(old, None, Some(&mut o))?
    } else {
        repo.diff_tree_to_tree(old, new, Some(&mut o))?
    })
}

fn full_diff<'r>(repo: &'r Repository, target: &DiffTarget) -> Result<Diff<'r>> {
    let mut d = match target {
        DiffTarget::Staged => {
            let h = head_tree(repo)?;
            tree_diff(repo, h.as_ref(), None, None, true)?
        }
        DiffTarget::Unstaged => {
            let mut o = base_opts(None, false);
            repo.diff_index_to_workdir(None, Some(&mut o))?
        }
        t => {
            let (old, new) = tree_pair(repo, t)?;
            tree_diff(repo, old.as_ref(), Some(&new), None, false)?
        }
    };
    find_renames(&mut d)?;
    Ok(d)
}

fn lossy(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    s.strip_suffix('\n').unwrap_or(&s).to_string()
}

pub(crate) fn delta_index(diff: &Diff<'_>, path: &str) -> Option<usize> {
    diff.deltas().position(|d| {
        d.new_file().path().is_some_and(|p| p.to_str() == Some(path))
            || d.old_file().path().is_some_and(|p| p.to_str() == Some(path))
    })
}

/// Lines of the old side's blob, empty when there is none (added or untracked).
fn old_side_lines(repo: &Repository, patch: &Patch<'_>) -> Vec<String> {
    let id = patch.delta().old_file().id();
    if id.is_zero() {
        return Vec::new();
    }
    match repo.find_blob(id) {
        Ok(b) => String::from_utf8_lossy(b.content()).lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

fn build(repo: &Repository, diff: &Diff<'_>, idx: usize, force_full: bool) -> Result<Option<FileDiff>> {
    let Some(patch) = Patch::from_diff(diff, idx)? else {
        return Ok(None);
    };
    let (is_bin, old_size, new_size) = {
        let d = patch.delta();
        (d.flags().is_binary(), d.old_file().size(), d.new_file().size())
    };
    let Some(entry) = entry_from_delta(patch.delta()) else {
        return Ok(None);
    };
    let mk = |body, additions, deletions, old_lines| FileDiff {
        path: entry.path.clone(),
        old_path: entry.old_path.clone(),
        status: entry.status,
        body,
        additions,
        deletions,
        old_lines,
    };
    if is_bin {
        return Ok(Some(mk(DiffBody::Binary { old_size, new_size }, 0, 0, Vec::new())));
    }
    let (ctx, add, del) = patch.line_stats()?;
    let total = ctx + add + del;
    if !force_full && (total > MAX_LINES || old_size.max(new_size) > MAX_BLOB_BYTES) {
        return Ok(Some(mk(DiffBody::TooLarge { lines: total }, add as u32, del as u32, Vec::new())));
    }
    let mut hunks = Vec::new();
    for h in 0..patch.num_hunks() {
        let (hd, n) = patch.hunk(h)?;
        let mut lines = Vec::with_capacity(n);
        for l in 0..n {
            let line = patch.line_in_hunk(h, l)?;
            let kind = match line.origin() {
                ' ' => LineKind::Context,
                '+' => LineKind::Added,
                '-' => LineKind::Removed,
                _ => continue,
            };
            lines.push(DiffLine {
                kind,
                old_no: line.old_lineno(),
                new_no: line.new_lineno(),
                text: lossy(line.content()),
            });
        }
        hunks.push(Hunk {
            header: String::from_utf8_lossy(hd.header()).trim().to_string(),
            old_start: hd.old_start(),
            old_lines: hd.old_lines(),
            new_start: hd.new_start(),
            new_lines: hd.new_lines(),
            lines,
        });
    }
    let old_lines = old_side_lines(repo, &patch);
    Ok(Some(mk(DiffBody::Text(hunks), add as u32, del as u32, old_lines)))
}

/// Diff of one path in a target, or None when the path has no change there.
///
/// Rename detection: the cheap pathspec diff runs first. Only when it reports the
/// path as Added (a possible rename target) do we diff the whole tree and run
/// `find_similar`, since a pathspec hides the old side. Unstaged diffs never
/// pair renames (index to workdir shows untracked files as such).
pub fn file_diff(
    repo: &Repository,
    target: &DiffTarget,
    path: &str,
    force_full: bool,
) -> Result<Option<FileDiff>> {
    let diff = match target {
        DiffTarget::Unstaged => unstaged_git_diff(repo, path, false)?,
        DiffTarget::Staged => staged_git_diff(repo, path, false)?,
        t => {
            let (old, new) = tree_pair(repo, t)?;
            tree_diff(repo, old.as_ref(), Some(&new), Some(path), false)?
        }
    };
    let Some(idx) = delta_index(&diff, path) else {
        return Ok(None);
    };
    let is_added = diff.get_delta(idx).is_some_and(|d| d.status() == git2::Delta::Added);
    if is_added && !matches!(target, DiffTarget::Unstaged) {
        let full = full_diff(repo, target)?;
        let hit = full
            .deltas()
            .position(|d| d.new_file().path().is_some_and(|p| p.to_str() == Some(path)));
        if let Some(i) = hit {
            return build(repo, &full, i, force_full);
        }
    }
    build(repo, &diff, idx, force_full)
}

pub fn files_in_target(repo: &Repository, target: &DiffTarget) -> Result<Vec<FileEntry>> {
    let diff = full_diff(repo, target)?;
    let mut out: Vec<FileEntry> = diff.deltas().filter_map(entry_from_delta).collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

pub fn diff_stats(repo: &Repository, target: &DiffTarget) -> Result<(u32, u32)> {
    let diff = full_diff(repo, target)?;
    let s = diff.stats()?;
    Ok((s.insertions() as u32, s.deletions() as u32))
}
