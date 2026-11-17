//! Git Flow configuration and state. Settings live in the standard
//! `gitflow.*` keys of the local repository config, so repositories
//! initialized with the git-flow command line tools are recognized.
use crate::branch::{checkout, head_info, HeadInfo};
use crate::{CoreError, Result};
use git2::{build::CheckoutBuilder, BranchType, Config, ConfigLevel, ErrorClass, ErrorCode, Reference, Repository};
use std::cmp::Ordering;

const KEY_PRODUCTION: &str = "gitflow.branch.master";
const KEY_DEVELOP: &str = "gitflow.branch.develop";
const KEY_FEATURE: &str = "gitflow.prefix.feature";
const KEY_RELEASE: &str = "gitflow.prefix.release";
const KEY_HOTFIX: &str = "gitflow.prefix.hotfix";
const KEY_VERSION_TAG: &str = "gitflow.prefix.versiontag";
const KEY_PUSH_DEFAULT: &str = "codediff.pushonfinish";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowKind {
    Feature,
    Release,
    Hotfix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowConfig {
    /// Production branch name, for example `main`.
    pub production: String,
    pub develop: String,
    /// Branch prefixes, for example `feature/`.
    pub feature: String,
    pub release: String,
    pub hotfix: String,
    /// Empty when there is none.
    pub version_tag: String,
}

impl FlowConfig {
    pub fn prefix(&self, kind: FlowKind) -> &str {
        match kind {
            FlowKind::Feature => &self.feature,
            FlowKind::Release => &self.release,
            FlowKind::Hotfix => &self.hotfix,
        }
    }

    /// develop for Feature and Release, production for Hotfix.
    pub fn base(&self, kind: FlowKind) -> &str {
        match kind {
            FlowKind::Feature | FlowKind::Release => &self.develop,
            FlowKind::Hotfix => &self.production,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagInfo {
    pub name: String,
    /// Unix seconds of the tagged commit.
    pub time: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowState {
    /// None when Git Flow is not initialized.
    pub config: Option<FlowConfig>,
    /// Values to pre-fill Initialize.
    pub draft: FlowConfig,
    pub init_blocked: Option<String>,
    /// Local branch names, for the production picker.
    pub branches: Vec<String>,
    /// Full branch names, newest tip first.
    pub features: Vec<String>,
    pub releases: Vec<String>,
    pub hotfixes: Vec<String>,
    /// HEAD is on this flow branch.
    pub current: Option<(FlowKind, String)>,
    pub latest_tag: Option<TagInfo>,
    pub push_blocked: Option<String>,
    pub push_default: bool,
}

fn get(cfg: &Config, key: &str) -> Result<Option<String>> {
    match cfg.get_string(key) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.code() == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn has_branch(repo: &Repository, name: &str) -> bool {
    repo.find_branch(name, BranchType::Local).is_ok()
}

/// Production, develop and the three prefixes as stored, each None when unset.
struct Stored {
    production: Option<String>,
    develop: Option<String>,
    feature: Option<String>,
    release: Option<String>,
    hotfix: Option<String>,
    version_tag: Option<String>,
}

fn read_stored(repo: &Repository) -> Result<Stored> {
    let cfg = repo.config()?.snapshot()?;
    Ok(Stored {
        production: get(&cfg, KEY_PRODUCTION)?,
        develop: get(&cfg, KEY_DEVELOP)?,
        feature: get(&cfg, KEY_FEATURE)?,
        release: get(&cfg, KEY_RELEASE)?,
        hotfix: get(&cfg, KEY_HOTFIX)?,
        version_tag: get(&cfg, KEY_VERSION_TAG)?,
    })
}

pub fn load_config(repo: &Repository) -> Result<Option<FlowConfig>> {
    let s = read_stored(repo)?;
    let (Some(production), Some(develop), Some(feature), Some(release), Some(hotfix)) =
        (s.production, s.develop, s.feature, s.release, s.hotfix)
    else {
        return Ok(None);
    };
    if production == develop || !has_branch(repo, &production) || !has_branch(repo, &develop) {
        return Ok(None);
    }
    Ok(Some(FlowConfig { production, develop, feature, release, hotfix, version_tag: s.version_tag.unwrap_or_default() }))
}

/// `main`, else `master`, else the branch HEAD is on.
fn default_production(repo: &Repository) -> Result<String> {
    for name in ["main", "master"] {
        if has_branch(repo, name) {
            return Ok(name.to_string());
        }
    }
    Ok(match head_info(repo)? {
        HeadInfo::Branch(b) => b,
        HeadInfo::Detached(_) => "main".to_string(),
    })
}

fn draft_config(repo: &Repository) -> Result<FlowConfig> {
    let s = read_stored(repo)?;
    let production = match s.production {
        Some(p) => p,
        None => default_production(repo)?,
    };
    Ok(FlowConfig {
        production,
        develop: s.develop.unwrap_or_else(|| "develop".into()),
        feature: s.feature.unwrap_or_else(|| "feature/".into()),
        release: s.release.unwrap_or_else(|| "release/".into()),
        hotfix: s.hotfix.unwrap_or_else(|| "hotfix/".into()),
        version_tag: s.version_tag.unwrap_or_default(),
    })
}

/// Local branches starting with `prefix`, newest tip first, ties by name.
fn branches_with_prefix(repo: &Repository, prefix: &str) -> Result<Vec<String>> {
    if prefix.is_empty() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for b in repo.branches(Some(BranchType::Local))? {
        let (b, _) = b?;
        let Ok(Some(name)) = b.name() else { continue };
        if !name.starts_with(prefix) {
            continue;
        }
        let Ok(commit) = b.get().peel_to_commit() else { continue };
        found.push((commit.time().seconds(), name.to_string()));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(found.into_iter().map(|(_, n)| n).collect())
}

pub fn flow_state(repo: &Repository, git_found: bool) -> Result<FlowState> {
    let config = load_config(repo)?;
    let draft = draft_config(repo)?;
    let unborn = matches!(repo.head(), Err(e) if e.code() == ErrorCode::UnbornBranch);
    let init_blocked = unborn.then(|| "Repository has no commits yet.".to_string());

    let mut branches = Vec::new();
    for b in repo.branches(Some(BranchType::Local))? {
        if let Ok(Some(name)) = b?.0.name() {
            branches.push(name.to_string());
        }
    }
    branches.sort();

    let (mut features, mut releases, mut hotfixes) = (Vec::new(), Vec::new(), Vec::new());
    let mut current = None;
    if let Some(cfg) = &config {
        features = branches_with_prefix(repo, &cfg.feature)?;
        releases = branches_with_prefix(repo, &cfg.release)?;
        hotfixes = branches_with_prefix(repo, &cfg.hotfix)?;
        if let HeadInfo::Branch(head) = head_info(repo)? {
            current = [FlowKind::Feature, FlowKind::Release, FlowKind::Hotfix]
                .into_iter()
                .find(|k| !cfg.prefix(*k).is_empty() && head.starts_with(cfg.prefix(*k)))
                .map(|k| (k, head));
        }
    }

    let latest_tag = match &config {
        Some(cfg) => latest_tag(repo, cfg)?,
        None => None,
    };

    let push_blocked = if repo.find_remote("origin").is_err() {
        Some("No origin remote.".to_string())
    } else if !git_found {
        Some("Git command line tools not found.".to_string())
    } else {
        None
    };
    let push_default = repo.config()?.snapshot()?.get_bool(KEY_PUSH_DEFAULT).unwrap_or(false);

    Ok(FlowState {
        config,
        draft,
        init_blocked,
        branches,
        features,
        releases,
        hotfixes,
        current,
        latest_tag,
        push_blocked,
        push_default,
    })
}

fn valid_prefix(prefix: &str) -> bool {
    !prefix.is_empty() && Reference::is_valid_name(&format!("refs/heads/{prefix}x"))
}

/// Validates `cfg`, creates the develop branch when missing, writes the
/// settings and checks out develop. A `CheckoutConflict` from that last step
/// is returned after the settings were written.
pub fn init(repo: &Repository, cfg: &FlowConfig) -> Result<()> {
    if cfg.production == cfg.develop {
        return Err(CoreError::Flow("Production and development branches must differ.".into()));
    }
    let production = repo
        .find_branch(&cfg.production, BranchType::Local)
        .map_err(|_| CoreError::Flow(format!("Production branch {} does not exist.", cfg.production)))?;
    for (label, prefix) in [("Feature", &cfg.feature), ("Release", &cfg.release), ("Hotfix", &cfg.hotfix)] {
        if !valid_prefix(prefix) {
            return Err(CoreError::Flow(format!("{label} prefix is not valid.")));
        }
    }

    if !has_branch(repo, &cfg.develop) {
        let upstream = format!("origin/{}", cfg.develop);
        match repo.find_reference(&format!("refs/remotes/{upstream}")) {
            Ok(r) => {
                let commit = r.peel_to_commit()?;
                let mut branch = repo.branch(&cfg.develop, &commit, false)?;
                branch.set_upstream(Some(&upstream))?;
            }
            Err(e) if e.code() == ErrorCode::NotFound => {
                repo.branch(&cfg.develop, &production.get().peel_to_commit()?, false)?;
            }
            Err(e) => return Err(e.into()),
        }
    }

    let mut local = repo.config()?.open_level(ConfigLevel::Local)?;
    for (key, value) in [
        (KEY_PRODUCTION, cfg.production.as_str()),
        (KEY_DEVELOP, &cfg.develop),
        (KEY_FEATURE, &cfg.feature),
        ("gitflow.prefix.bugfix", "bugfix/"),
        (KEY_RELEASE, &cfg.release),
        (KEY_HOTFIX, &cfg.hotfix),
        ("gitflow.prefix.support", "support/"),
        (KEY_VERSION_TAG, &cfg.version_tag),
    ] {
        local.set_str(key, value)?;
    }

    checkout(repo, &format!("refs/heads/{}", cfg.develop))
}

pub fn set_push_default(repo: &Repository, on: bool) -> Result<()> {
    repo.config()?.open_level(ConfigLevel::Local)?.set_bool(KEY_PUSH_DEFAULT, on)?;
    Ok(())
}

/// Why this start is refused, or None when it can run.
pub fn start_error(repo: &Repository, cfg: &FlowConfig, kind: FlowKind, name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("Enter a name.".into());
    }
    let branch = format!("{}{}", cfg.prefix(kind), name);
    if name.contains(char::is_whitespace)
        || name.starts_with('/')
        || name.ends_with('/')
        || !matches!(git2::Branch::name_is_valid(&branch), Ok(true))
    {
        return Some("Not a valid branch name.".into());
    }
    if has_branch(repo, &branch) {
        return Some(format!("{branch} already exists."));
    }
    if repo.find_reference(&format!("refs/remotes/origin/{branch}")).is_ok() {
        return Some(format!("{branch} already exists on origin."));
    }
    // `a` and `a/b` cannot both exist: one would be a file and the other a
    // directory under refs/heads. Caught here, before the checkout runs.
    if let Some(other) = nested_branch(repo, &branch) {
        return Some(format!("{branch} conflicts with existing branch {other}."));
    }
    let (label, open) = match kind {
        FlowKind::Feature => return None,
        FlowKind::Release => ("release", &cfg.release),
        FlowKind::Hotfix => ("hotfix", &cfg.hotfix),
    };
    let tag = format!("{}{}", cfg.version_tag, name);
    if repo.find_reference(&format!("refs/tags/{tag}")).is_ok() {
        return Some(format!("Tag {tag} already exists."));
    }
    let open_branch = branches_with_prefix(repo, open).ok()?.into_iter().next()?;
    Some(format!("A {label} is already open: {open_branch}."))
}

/// A local branch that `branch` would nest in, or that would nest in it.
fn nested_branch(repo: &Repository, branch: &str) -> Option<String> {
    let inside = |name: &str, dir: &str| name.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'));
    repo.branches(Some(BranchType::Local))
        .ok()?
        .flatten()
        .filter_map(|(b, _)| b.name().ok().flatten().map(str::to_string))
        .find(|other| inside(branch, other) || inside(other, branch))
}

/// Creates `prefix + name` from the base branch of `kind` and switches to it.
/// Like `branch::checkout`: a safe tree checkout first, so a refusal leaves
/// everything untouched, then the branch, then HEAD.
pub fn start(repo: &Repository, cfg: &FlowConfig, kind: FlowKind, name: &str) -> Result<()> {
    if let Some(msg) = start_error(repo, cfg, kind, name) {
        return Err(CoreError::Flow(msg));
    }
    let branch = format!("{}{}", cfg.prefix(kind), name);
    let base = repo.find_branch(cfg.base(kind), BranchType::Local)?;
    let commit = base.get().peel_to_commit()?;
    repo.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().safe())).map_err(|e| {
        if e.class() == ErrorClass::Checkout && e.code() == ErrorCode::Conflict {
            CoreError::CheckoutConflict
        } else {
            e.into()
        }
    })?;
    repo.branch(&branch, &commit, false)?;
    repo.set_head(&format!("refs/heads/{branch}"))?;
    Ok(())
}

/// Compares dotted versions number by number; non-numeric parts compare as text.
fn compare_versions(a: &str, b: &str) -> Ordering {
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    _ => x.cmp(y),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// The newest tag with the version tag prefix whose commit is the production
/// tip or one of its ancestors. Candidates are tried newest first (ties: the
/// higher version), so the graph is walked only until the first reachable one.
pub fn latest_tag(repo: &Repository, cfg: &FlowConfig) -> Result<Option<TagInfo>> {
    let tip = repo.find_branch(&cfg.production, BranchType::Local)?.get().peel_to_commit()?.id();
    let mut candidates: Vec<(TagInfo, git2::Oid)> = Vec::new();
    for name in repo.tag_names(None)?.iter().flatten().flatten() {
        if !name.starts_with(&cfg.version_tag) {
            continue;
        }
        let Ok(commit) = repo.find_reference(&format!("refs/tags/{name}")).and_then(|r| r.peel_to_commit()) else {
            continue;
        };
        candidates.push((TagInfo { name: name.to_string(), time: commit.time().seconds() }, commit.id()));
    }
    // Newest first by time only: `compare_versions` is not a total order
    // (`1.1a` < `1.9` < `1.10` < `1.1a`), and a sort panics on such a
    // comparator. It only picks among the reachable tags of one commit time.
    candidates.sort_by_key(|(info, _)| std::cmp::Reverse(info.time));
    for same_time in candidates.chunk_by(|(a, _), (b, _)| a.time == b.time) {
        let mut best: Option<&TagInfo> = None;
        for (info, id) in same_time {
            if *id != tip && !repo.graph_descendant_of(tip, *id)? {
                continue;
            }
            if best.is_none_or(|b| compare_versions(&info.name, &b.name) == Ordering::Greater) {
                best = Some(info);
            }
        }
        if let Some(info) = best {
            return Ok(Some(info.clone()));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowAction {
    pub key: String,
    pub name: String,
    pub detail: String,
}

fn action(key: impl Into<String>, name: impl Into<String>, detail: impl Into<String>) -> FlowAction {
    FlowAction { key: key.into(), name: name.into(), detail: detail.into() }
}

fn finish_action(cfg: &FlowConfig, kind: FlowKind, branch: &str, current: bool) -> FlowAction {
    let (word, key, detail) = match kind {
        FlowKind::Feature => ("feature", "feature", format!("Merge into {}", cfg.develop)),
        FlowKind::Release => ("release", "release", format!("Merge into {} and {}, tag", cfg.production, cfg.develop)),
        FlowKind::Hotfix => ("hotfix", "hotfix", format!("Merge into {} and {}, tag", cfg.production, cfg.develop)),
    };
    let short = branch.strip_prefix(cfg.prefix(kind)).unwrap_or(branch);
    let detail = if current { format!("{detail} \u{b7} current branch") } else { detail };
    action(format!("finish:{key}:{branch}"), format!("Finish {word} {short}"), detail)
}

/// The rows of the Git Flow actions list, most relevant first.
pub fn next_actions(state: &FlowState) -> Vec<FlowAction> {
    let Some(cfg) = &state.config else {
        return vec![action("init", "Initialize repository", "Set up Git Flow branches and prefixes")];
    };
    let mut rows = Vec::new();
    let current_branch = state.current.as_ref().map(|(_, b)| b.as_str());
    if let Some((kind, branch)) = &state.current {
        rows.push(finish_action(cfg, *kind, branch, true));
    }
    for (kind, open) in [(FlowKind::Release, &state.releases), (FlowKind::Hotfix, &state.hotfixes)] {
        if let Some(branch) = open.first().filter(|b| Some(b.as_str()) != current_branch) {
            rows.push(finish_action(cfg, kind, branch, false));
        }
    }
    let on_feature = matches!(state.current, Some((FlowKind::Feature, _)));
    if !state.features.is_empty() && !on_feature {
        rows.push(action("finish:feature", "Finish feature\u{2026}", format!("Pick a feature to merge into {}", cfg.develop)));
    }
    rows.push(action("start:feature", "Start new feature", format!("Branch from {}", cfg.develop)));
    if state.releases.is_empty() {
        rows.push(action("start:release", "Start new release", format!("Branch from {}", cfg.develop)));
    }
    if state.hotfixes.is_empty() {
        rows.push(action("start:hotfix", "Start new hotfix", format!("Branch from {}", cfg.production)));
    }
    rows
}
