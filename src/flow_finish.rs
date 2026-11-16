//! Git Flow finish. A finish is all or nothing, so it is planned first:
//! every merge runs in memory and anything that would stop it part way is
//! reported as a blocker before a reference, HEAD, the index or the working
//! tree is touched. Planning may write unreferenced objects, nothing else.
use crate::flow::{FlowConfig, FlowKind};
use crate::model::FileStatus;
use crate::status::list_status;
use crate::Result;
use git2::{BranchType, Delta, Oid, Reference, Repository, RepositoryState};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishRequest {
    pub kind: FlowKind,
    /// Full local name, for example `release/1.2.0`.
    pub branch: String,
    /// Empty for a feature.
    pub tag: String,
    /// Tag message, empty for a feature.
    pub message: String,
    pub delete_branch: bool,
    /// Ignored for a feature.
    pub push: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    /// Number of changed tracked files.
    Dirty(usize),
    Busy,
    NoIdentity,
    OtherWorktree { branch: String, path: PathBuf },
    BehindOrigin { branch: String },
    TagInvalid,
    TagExists(String),
    Conflict { target: String, branch: String, files: Vec<String> },
    Collision(Vec<String>),
}

impl Blocker {
    pub fn title(&self) -> String {
        match self {
            Blocker::Dirty(1) => "1 uncommitted change".into(),
            Blocker::Dirty(n) => format!("{n} uncommitted changes"),
            Blocker::Busy => "Repository is busy".into(),
            Blocker::NoIdentity => "Git identity not set".into(),
            Blocker::OtherWorktree { branch, .. } => format!("{branch} is checked out in another worktree"),
            Blocker::BehindOrigin { branch } => format!("{branch} is behind origin"),
            Blocker::TagInvalid => "Tag name is not valid".into(),
            Blocker::TagExists(tag) => format!("Tag {tag} already exists"),
            Blocker::Conflict { .. } => "Cannot merge cleanly".into(),
            Blocker::Collision(_) => "Untracked files are in the way".into(),
        }
    }

    pub fn body(&self) -> String {
        match self {
            Blocker::Dirty(_) => {
                "Finish needs a clean working tree. Commit them, or discard them in Changes, then try again.".into()
            }
            Blocker::Busy => "A merge, rebase or cherry-pick is in progress. Finish it first.".into(),
            Blocker::NoIdentity => "Set user.name and user.email in Git config, then try again.".into(),
            Blocker::OtherWorktree { path, .. } => {
                format!("{}. Switch that worktree to another branch first.", path.display())
            }
            Blocker::BehindOrigin { branch } => format!("Pull {branch} first, then finish again."),
            Blocker::TagInvalid | Blocker::TagExists(_) => "Pick another tag name.".into(),
            Blocker::Conflict { target, branch, .. } => format!(
                "{target} and {branch} both changed these files. Nothing was changed. \
                 Merge {target} into {branch} first (agent or terminal), then finish again."
            ),
            Blocker::Collision(_) => "Move or delete them, then try again.".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub text: String,
    pub note: String,
    /// The step runs on origin.
    pub remote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishPlan {
    pub steps: Vec<Step>,
    pub blocker: Option<Blocker>,
}

/// One merge computed in memory.
pub(crate) enum Merge {
    /// The target already contains the source.
    AlreadyMerged,
    /// The tree the merge commit would have.
    Clean(Oid),
    /// Conflicting paths, sorted, each once.
    Conflict(Vec<String>),
}

/// Merges `source` into `target` in memory. Writes at most a tree object.
pub(crate) fn test_merge(repo: &Repository, target: Oid, source: Oid) -> Result<Merge> {
    if target == source || repo.graph_descendant_of(target, source)? {
        return Ok(Merge::AlreadyMerged);
    }
    let mut index = repo.merge_commits(&repo.find_commit(target)?, &repo.find_commit(source)?, None)?;
    if index.has_conflicts() {
        let mut files = BTreeSet::new();
        for c in index.conflicts()? {
            let c = c?;
            if let Some(e) = c.our.or(c.their).or(c.ancestor) {
                files.insert(String::from_utf8_lossy(&e.path).into_owned());
            }
        }
        return Ok(Merge::Conflict(files.into_iter().collect()));
    }
    Ok(Merge::Clean(index.write_tree_to(repo)?))
}

pub(crate) fn merge_message(source: &str, target: &str) -> String {
    format!("Merge branch '{source}' into {target}")
}

/// What the merges of a finish would produce.
pub(crate) enum Simulation {
    Clean {
        /// The new production tip for a release or hotfix: an unreferenced
        /// merge commit, or the current tip when it already contains the
        /// branch. None for a feature. Read by the executing finish.
        #[allow(dead_code)]
        production: Option<Oid>,
        /// The tree develop would have after the finish.
        develop_tree: Oid,
    },
    Conflict { target: String, files: Vec<String> },
}

fn tip(repo: &Repository, branch: &str) -> Result<Oid> {
    Ok(repo.find_branch(branch, BranchType::Local)?.get().peel_to_commit()?.id())
}

/// Runs every merge of the finish in memory. Needs a Git identity for the
/// production merge commit of a release or hotfix.
pub(crate) fn simulate(repo: &Repository, cfg: &FlowConfig, req: &FinishRequest) -> Result<Simulation> {
    let branch_tip = tip(repo, &req.branch)?;
    let develop_tip = tip(repo, &cfg.develop)?;
    let tree_of = |id: Oid| -> Result<Oid> { Ok(repo.find_commit(id)?.tree_id()) };

    let (production, into_develop) = match req.kind {
        FlowKind::Feature => (None, branch_tip),
        FlowKind::Release | FlowKind::Hotfix => {
            let production_tip = tip(repo, &cfg.production)?;
            let new_tip = match test_merge(repo, production_tip, branch_tip)? {
                Merge::AlreadyMerged => production_tip,
                Merge::Conflict(files) => {
                    return Ok(Simulation::Conflict { target: cfg.production.clone(), files });
                }
                Merge::Clean(tree) => {
                    let sig = repo.signature()?;
                    let parents = [&repo.find_commit(production_tip)?, &repo.find_commit(branch_tip)?];
                    let message = merge_message(&req.branch, &cfg.production);
                    repo.commit(None, &sig, &sig, &message, &repo.find_tree(tree)?, &parents)?
                }
            };
            (Some(new_tip), new_tip)
        }
    };

    let develop_tree = match test_merge(repo, develop_tip, into_develop)? {
        Merge::AlreadyMerged => tree_of(develop_tip)?,
        Merge::Clean(tree) => tree,
        Merge::Conflict(files) => return Ok(Simulation::Conflict { target: cfg.develop.clone(), files }),
    };
    Ok(Simulation::Clean { production, develop_tree })
}

fn step(text: String, note: String, remote: bool) -> Step {
    Step { text, note, remote }
}

fn steps(repo: &Repository, cfg: &FlowConfig, req: &FinishRequest) -> Result<Vec<Step>> {
    let b = &req.branch;
    let first_target = match req.kind {
        FlowKind::Feature => &cfg.develop,
        FlowKind::Release | FlowKind::Hotfix => &cfg.production,
    };
    let (ahead, _) = repo.graph_ahead_behind(tip(repo, b)?, tip(repo, first_target)?)?;
    let note = match ahead {
        0 => "already merged".to_string(),
        1 => "1 commit".to_string(),
        n => format!("{n} commits"),
    };

    let mut out = vec![step(format!("Merge {b} into {first_target}"), note, false)];
    let feature = req.kind == FlowKind::Feature;
    if !feature {
        out.push(step(format!("Tag {} as {}", cfg.production, req.tag), String::new(), false));
        out.push(step(format!("Merge {b} back into {}", cfg.develop), String::new(), false));
    }
    if req.delete_branch {
        out.push(step(format!("Delete {b}"), String::new(), false));
    }
    out.push(step(format!("Switch to {}", cfg.develop), String::new(), false));
    if !feature && req.push {
        let text = format!("Push {}, {} and tag {} to origin", cfg.production, cfg.develop, req.tag);
        out.push(step(text, String::new(), true));
        if req.delete_branch && repo.find_reference(&format!("refs/remotes/origin/{b}")).is_ok() {
            out.push(step(format!("Delete {b} on origin"), String::new(), true));
        }
    }
    Ok(out)
}

/// Distinct tracked paths with staged or unstaged changes.
fn dirty_count(repo: &Repository) -> Result<usize> {
    let status = list_status(repo)?;
    let paths: BTreeSet<&str> = status
        .staged
        .iter()
        .chain(&status.unstaged)
        .filter(|e| e.status != FileStatus::Untracked)
        .map(|e| e.path.as_str())
        .collect();
    Ok(paths.len())
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Branch HEAD of `r` points at, None when detached.
fn checked_out_branch(r: &Repository) -> Option<String> {
    let head = r.find_reference("HEAD").ok()?;
    head.symbolic_target().ok()??.strip_prefix("refs/heads/").map(str::to_string)
}

/// Branch and working directory of every worktree other than this one.
fn other_worktrees(repo: &Repository) -> Result<Vec<(String, PathBuf)>> {
    let mut repos = Vec::new();
    if repo.is_worktree() {
        repos.push(Repository::open(repo.commondir())?);
    }
    for name in repo.worktrees()?.iter().flatten().flatten() {
        let wt = repo.find_worktree(name)?;
        if wt.validate().is_err() {
            continue;
        }
        repos.push(Repository::open_from_worktree(&wt)?);
    }
    let me = canonical(repo.path());
    Ok(repos
        .iter()
        .filter(|r| canonical(r.path()) != me)
        .filter_map(|r| Some((checked_out_branch(r)?, r.workdir()?.to_path_buf())))
        .collect())
}

fn behind_origin(repo: &Repository, branch: &str) -> Result<bool> {
    let Ok(remote) = repo.find_reference(&format!("refs/remotes/origin/{branch}")) else {
        return Ok(false);
    };
    let (_, behind) = repo.graph_ahead_behind(tip(repo, branch)?, remote.peel_to_commit()?.id())?;
    Ok(behind > 0)
}

/// Untracked files a safe checkout of `tree` would refuse to overwrite: those
/// at a path the tree brings in, or where it needs a directory or a file.
/// Computed from a diff, since git2 offers no conflict-checking dry run
/// (its `dry_run` maps to `GIT_CHECKOUT_NONE`, which skips conflict checks).
fn collisions(repo: &Repository, tree: Oid) -> Result<Vec<String>> {
    let status = list_status(repo)?;
    let untracked: Vec<&str> = status
        .unstaged
        .iter()
        .filter(|e| e.status == FileStatus::Untracked)
        .map(|e| e.path.as_str())
        .collect();
    if untracked.is_empty() {
        return Ok(Vec::new());
    }
    let head_tree = repo.head()?.peel_to_tree()?;
    let diff = repo.diff_tree_to_tree(Some(&head_tree), Some(&repo.find_tree(tree)?), None)?;
    let incoming: Vec<String> = diff
        .deltas()
        .filter(|d| d.status() != Delta::Deleted)
        .filter_map(|d| Some(d.new_file().path()?.to_string_lossy().into_owned()))
        .collect();
    let inside = |path: &str, dir: &str| path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'));
    let found: BTreeSet<String> = untracked
        .into_iter()
        .filter(|u| incoming.iter().any(|a| a == u || inside(a, u) || inside(u, a)))
        .map(str::to_string)
        .collect();
    Ok(found.into_iter().collect())
}

fn find_blocker(repo: &Repository, cfg: &FlowConfig, req: &FinishRequest) -> Result<Option<Blocker>> {
    let dirty = dirty_count(repo)?;
    if dirty > 0 {
        return Ok(Some(Blocker::Dirty(dirty)));
    }
    if repo.state() != RepositoryState::Clean {
        return Ok(Some(Blocker::Busy));
    }
    if repo.signature().is_err() {
        return Ok(Some(Blocker::NoIdentity));
    }

    let release = req.kind != FlowKind::Feature;
    let mut touched = vec![cfg.develop.as_str()];
    if release {
        touched.push(&cfg.production);
    }
    let others = other_worktrees(repo)?;
    let deleted = req.delete_branch.then_some(req.branch.as_str());
    for branch in touched.iter().copied().chain(deleted) {
        if let Some((_, path)) = others.iter().find(|(b, _)| b == branch) {
            return Ok(Some(Blocker::OtherWorktree { branch: branch.into(), path: path.clone() }));
        }
    }
    for branch in touched.iter().copied().chain([req.branch.as_str()]) {
        if behind_origin(repo, branch)? {
            return Ok(Some(Blocker::BehindOrigin { branch: branch.into() }));
        }
    }

    if release {
        if req.tag.is_empty() || !Reference::is_valid_name(&format!("refs/tags/{}", req.tag)) {
            return Ok(Some(Blocker::TagInvalid));
        }
        if repo.find_reference(&format!("refs/tags/{}", req.tag)).is_ok() {
            return Ok(Some(Blocker::TagExists(req.tag.clone())));
        }
    }

    match simulate(repo, cfg, req)? {
        Simulation::Conflict { target, files } => {
            Ok(Some(Blocker::Conflict { target, branch: req.branch.clone(), files }))
        }
        Simulation::Clean { develop_tree, .. } => {
            let paths = collisions(repo, develop_tree)?;
            Ok((!paths.is_empty()).then_some(Blocker::Collision(paths)))
        }
    }
}

/// The steps a finish would run and the first thing that blocks it.
/// Changes no reference, HEAD, index or working tree.
pub fn plan_finish(repo: &Repository, cfg: &FlowConfig, req: &FinishRequest) -> Result<FinishPlan> {
    Ok(FinishPlan { steps: steps(repo, cfg, req)?, blocker: find_blocker(repo, cfg, req)? })
}
