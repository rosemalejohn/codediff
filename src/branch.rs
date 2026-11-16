//! Current branch, branch listing and branch checkout.
use crate::{CoreError, Result};
use git2::{build::CheckoutBuilder, BranchType, ErrorClass, ErrorCode, Repository};

/// What HEAD points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadInfo {
    /// On a branch (possibly unborn, with no commits yet).
    Branch(String),
    /// Detached at a commit; holds the short id.
    Detached(String),
}

/// One checkout target for the branch switcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    /// Display name: `main` for a local branch, `origin/feature` for a remote one.
    pub name: String,
    /// Full reference name, the value `checkout` takes.
    pub refname: String,
    pub remote: bool,
    /// The branch HEAD is on.
    pub current: bool,
    /// Summary of the branch's tip commit.
    pub summary: String,
    /// Tip commit time, unix seconds.
    pub time: i64,
}

pub fn head_info(repo: &Repository) -> Result<HeadInfo> {
    match repo.head() {
        Ok(h) if h.is_branch() => Ok(HeadInfo::Branch(String::from_utf8_lossy(h.shorthand_bytes()).into_owned())),
        Ok(h) => {
            let id = h.peel_to_commit()?.id().to_string();
            Ok(HeadInfo::Detached(id[..7].to_string()))
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch => {
            let head = repo.find_reference("HEAD")?;
            let target = String::from_utf8_lossy(head.symbolic_target_bytes().unwrap_or_default()).into_owned();
            Ok(HeadInfo::Branch(target.trim_start_matches("refs/heads/").to_string()))
        }
        Err(e) => Err(e.into()),
    }
}

/// Local branches, then remote branches with no local branch of the same
/// name; each group newest tip first.
pub fn list_branches(repo: &Repository) -> Result<Vec<BranchInfo>> {
    let current = match head_info(repo)? {
        HeadInfo::Branch(name) => Some(name),
        HeadInfo::Detached(_) => None,
    };
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for b in repo.branches(None)? {
        let (b, kind) = b?;
        let r = b.get();
        // Skip symbolic refs such as origin/HEAD.
        if r.symbolic_target_bytes().is_some() {
            continue;
        }
        let (Ok(Some(name)), Ok(refname)) = (b.name(), r.name()) else { continue };
        let Ok(commit) = r.peel_to_commit() else { continue };
        let info = BranchInfo {
            name: name.to_string(),
            refname: refname.to_string(),
            remote: kind == BranchType::Remote,
            current: kind == BranchType::Local && current.as_deref() == Some(name),
            summary: String::from_utf8_lossy(commit.summary_bytes().unwrap_or_default()).into_owned(),
            time: commit.time().seconds(),
        };
        match kind {
            BranchType::Local => local.push(info),
            BranchType::Remote => remote.push(info),
        }
    }
    remote.retain(|r| {
        let short = remote_short_name(repo, &r.refname).unwrap_or_default();
        !local.iter().any(|l| l.name == short)
    });
    local.sort_by(|a, b| b.time.cmp(&a.time));
    remote.sort_by(|a, b| b.time.cmp(&a.time));
    local.extend(remote);
    Ok(local)
}

/// `feature` for `refs/remotes/origin/feature`.
fn remote_short_name(repo: &Repository, refname: &str) -> Option<String> {
    let remote = repo.branch_remote_name(refname).ok()?;
    let remote = remote.as_str().ok()?;
    refname
        .strip_prefix("refs/remotes/")?
        .strip_prefix(remote)?
        .strip_prefix('/')
        .map(str::to_string)
}

/// Checks out `refname` (a `refs/heads/` or `refs/remotes/` name). A remote
/// branch gets a local branch of the same short name that tracks it. Refuses,
/// like `git checkout`, when local changes would be overwritten.
pub fn checkout(repo: &Repository, refname: &str) -> Result<()> {
    let reference = repo.find_reference(refname)?;
    let commit = reference.peel_to_commit()?;
    let is_remote = refname.starts_with("refs/remotes/");
    let local = if is_remote {
        let short = remote_short_name(repo, refname)
            .ok_or_else(|| git2::Error::from_str(&format!("Not a remote branch: {refname}")))?;
        match repo.find_branch(&short, BranchType::Local) {
            Ok(_) => return checkout(repo, &format!("refs/heads/{short}")),
            Err(e) if e.code() == ErrorCode::NotFound => Some(short),
            Err(e) => return Err(e.into()),
        }
    } else {
        None
    };
    repo.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().safe()))
        .map_err(|e| {
            if e.class() == ErrorClass::Checkout && e.code() == ErrorCode::Conflict {
                CoreError::CheckoutConflict
            } else {
                e.into()
            }
        })?;
    let head = match local {
        Some(short) => {
            let mut branch = repo.branch(&short, &commit, false)?;
            let upstream = refname.trim_start_matches("refs/remotes/");
            branch.set_upstream(Some(upstream))?;
            format!("refs/heads/{short}")
        }
        None => refname.to_string(),
    };
    repo.set_head(&head)?;
    Ok(())
}
