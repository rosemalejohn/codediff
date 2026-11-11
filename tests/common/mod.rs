#![allow(dead_code)]
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
}
