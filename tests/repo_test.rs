mod common;
use codediff::recent::RecentRepos;
use codediff::repo::open_repo;
use codediff::CoreError;
use common::*;
use std::path::PathBuf;

#[test]
fn open_valid_repo() {
    let t = TestRepo::new();
    assert!(open_repo(t.path()).is_ok());
}

#[test]
fn open_non_repo_errors() {
    let d = tempfile::tempdir().unwrap();
    let e = open_repo(d.path()).err().expect("should fail");
    assert!(matches!(e, CoreError::NotARepo(_)));
    assert!(e.to_string().starts_with("Not a Git repository: "));
}

#[test]
fn recent_touch_dedupes_and_caps() {
    let mut r = RecentRepos::default();
    for i in 0..12 {
        r.touch(PathBuf::from(format!("/r{i}")));
    }
    r.touch(PathBuf::from("/r5"));
    assert_eq!(r.paths.len(), 10);
    assert_eq!(r.paths[0], PathBuf::from("/r5"));
    assert_eq!(r.paths.iter().filter(|p| **p == PathBuf::from("/r5")).count(), 1);
}

#[test]
fn recent_drops_missing_paths() {
    let t = TestRepo::new();
    let f = tempfile::NamedTempFile::new().unwrap();
    let r = RecentRepos { paths: vec![t.path().into(), "/definitely/not/here".into()] };
    r.save_to(f.path()).unwrap();
    assert_eq!(RecentRepos::load_from(f.path()).paths, vec![t.path().to_path_buf()]);
}

#[test]
fn recent_corrupt_file_is_empty() {
    let f = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "{oops").unwrap();
    assert!(RecentRepos::load_from(f.path()).paths.is_empty());
}

#[test]
fn test_repo_commits_with_parents() {
    let t = TestRepo::new();
    t.write("a.txt", b"1");
    t.stage("a.txt");
    let c1 = t.commit("one");
    t.write("a.txt", b"2");
    t.stage("a.txt");
    let c2 = t.commit("two");
    assert_ne!(c1, c2);
    assert_eq!(t.repo.find_commit(c2).unwrap().parent_count(), 1);
    assert_eq!(t.read("a.txt"), b"2");
}
