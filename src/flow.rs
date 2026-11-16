//! Git Flow configuration and state. Settings live in the standard
//! `gitflow.*` keys of the local repository config, so repositories
//! initialized with the git-flow command line tools are recognized.
use crate::branch::{checkout, head_info, HeadInfo};
use crate::{CoreError, Result};
use git2::{BranchType, Config, ConfigLevel, ErrorCode, Reference, Repository};

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
        latest_tag: None,
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
