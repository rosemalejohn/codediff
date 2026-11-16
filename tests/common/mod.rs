#![allow(dead_code)]
use codediff::flow::{init, FlowConfig, FlowKind};
use codediff::flow_finish::FinishRequest;
use git2::{Oid, Repository};
use std::path::Path;
use tempfile::TempDir;

pub struct TestRepo {
    pub dir: TempDir,
    pub repo: Repository,
}

impl TestRepo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Test").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        TestRepo { dir, repo }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn write(&self, rel: &str, contents: &[u8]) {
        let p = self.path().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, contents).unwrap();
    }

    pub fn read(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.path().join(rel)).unwrap()
    }

    pub fn stage(&self, rel: &str) {
        let mut idx = self.repo.index().unwrap();
        idx.add_path(Path::new(rel)).unwrap();
        idx.write().unwrap();
    }

    /// Bytes of the stage-0 index entry for `rel`, or None when not in the index.
    pub fn index_blob(&self, rel: &str) -> Option<Vec<u8>> {
        let mut idx = self.repo.index().unwrap();
        idx.read(false).unwrap();
        let e = idx.get_path(Path::new(rel), 0)?;
        Some(self.repo.find_blob(e.id).unwrap().content().to_vec())
    }

    /// Bytes of `rel` in the HEAD commit tree, or None when absent.
    pub fn head_blob(&self, rel: &str) -> Option<Vec<u8>> {
        let tree = self.repo.head().ok()?.peel_to_tree().ok()?;
        let e = tree.get_path(Path::new(rel)).ok()?;
        Some(self.repo.find_blob(e.id()).unwrap().content().to_vec())
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.path().join(rel).exists()
    }

    pub fn remove(&self, rel: &str) {
        std::fs::remove_file(self.path().join(rel)).unwrap();
    }

    pub fn commit(&self, msg: &str) -> Oid {
        let mut idx = self.repo.index().unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = self.repo.find_tree(tree_id).unwrap();
        let sig = self.repo.signature().unwrap();
        let parent = self.repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        self.repo
            .commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
            .unwrap()
    }

    /// Like `commit`, with an explicit author and committer time in unix seconds.
    pub fn commit_at(&self, msg: &str, secs: i64) -> Oid {
        let mut idx = self.repo.index().unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = self.repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::new("Test", "test@example.com", &git2::Time::new(secs, 0)).unwrap();
        let parent = self.repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        self.repo
            .commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
            .unwrap()
    }

    /// Write, stage and commit one file.
    pub fn commit_file(&self, rel: &str, contents: &[u8], msg: &str) -> Oid {
        self.write(rel, contents);
        self.stage(rel);
        self.commit(msg)
    }

    /// New branch at HEAD, not checked out.
    pub fn branch(&self, name: &str) {
        let head = self.repo.head().unwrap().peel_to_commit().unwrap();
        self.repo.branch(name, &head, false).unwrap();
    }

    pub fn checkout(&self, name: &str) {
        codediff::branch::checkout(&self.repo, &format!("refs/heads/{name}")).unwrap();
    }

    pub fn tip(&self, name: &str) -> Oid {
        self.repo.find_reference(&format!("refs/heads/{name}")).unwrap().target().unwrap()
    }

    pub fn head_branch(&self) -> String {
        match codediff::branch::head_info(&self.repo).unwrap() {
            codediff::branch::HeadInfo::Branch(b) => b,
            codediff::branch::HeadInfo::Detached(id) => panic!("detached at {id}"),
        }
    }

    pub fn has_branch(&self, name: &str) -> bool {
        self.repo.find_branch(name, git2::BranchType::Local).is_ok()
    }
}

/// Default Git Flow settings with `production` as the production branch.
pub fn default_config(production: &str) -> FlowConfig {
    FlowConfig {
        production: production.into(),
        develop: "develop".into(),
        feature: "feature/".into(),
        release: "release/".into(),
        hotfix: "hotfix/".into(),
        version_tag: String::new(),
    }
}

/// One commit on the initial branch, Git Flow initialized with defaults, HEAD on develop.
pub fn flow_repo() -> (TestRepo, FlowConfig) {
    let t = TestRepo::new();
    t.commit_file("a.txt", b"one\n", "first");
    let cfg = default_config(&t.head_branch());
    init(&t.repo, &cfg).unwrap();
    (t, cfg)
}

/// Finish request for a feature: no tag, delete the branch, no push.
pub fn feature_req(branch: &str) -> FinishRequest {
    FinishRequest {
        kind: FlowKind::Feature,
        branch: branch.into(),
        tag: String::new(),
        message: String::new(),
        delete_branch: true,
        push: false,
    }
}

/// Finish request for a release: delete the branch, no push.
pub fn release_req(branch: &str, tag: &str, message: &str) -> FinishRequest {
    FinishRequest {
        kind: FlowKind::Release,
        branch: branch.into(),
        tag: tag.into(),
        message: message.into(),
        delete_branch: true,
        push: false,
    }
}

/// Run the real `git` in `dir` with a fixed identity; panics unless it succeeds.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .output()
        .expect("git must be installed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A `flow_repo` with a bare repository as `origin` (the returned `TempDir`),
/// production and develop already pushed to it.
pub fn with_origin() -> (TestRepo, FlowConfig, TempDir) {
    let (t, cfg) = flow_repo();
    let bare = tempfile::tempdir().unwrap();
    git(bare.path(), &["init", "--bare"]);
    t.repo.remote("origin", bare.path().to_str().unwrap()).unwrap();
    git(t.path(), &["push", "origin", &cfg.production, "develop"]);
    (t, cfg, bare)
}

/// Target of `name` (a full reference name) in the bare repository.
pub fn origin_ref(bare: &TempDir, name: &str) -> Option<Oid> {
    let repo = Repository::open_bare(bare.path()).unwrap();
    let id = repo.find_reference(name).ok()?.target();
    id
}
