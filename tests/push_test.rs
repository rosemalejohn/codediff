mod common;
use codediff::flow::{start, FlowKind};
use codediff::flow_finish::{finish, PushSpec};
use codediff::push::{find_git, push, push_args};
use common::*;
use git2::Oid;
use std::path::PathBuf;

fn git_bin() -> PathBuf {
    find_git().expect("git must be installed to run these tests")
}

/// Start release/1.2.0 with one commit and push it to origin.
fn release_on_origin(t: &TestRepo, cfg: &codediff::flow::FlowConfig) {
    start(&t.repo, cfg, FlowKind::Release, "1.2.0").unwrap();
    t.commit_file("r.txt", b"r\n", "release work");
    git(t.path(), &["push", "origin", "release/1.2.0"]);
    t.checkout("develop");
}

fn spec_for(t: &TestRepo, cfg: &codediff::flow::FlowConfig) -> PushSpec {
    let mut req = release_req("release/1.2.0", "1.2.0", "Release 1.2.0");
    req.push = true;
    finish(&t.repo, cfg, &req).unwrap().push.expect("push spec")
}

fn clone_of(bare: &tempfile::TempDir) -> tempfile::TempDir {
    let c = tempfile::tempdir().unwrap();
    git(c.path(), &["clone", bare.path().to_str().unwrap(), "."]);
    c
}

#[test]
fn push_args_shape() {
    let oid = Oid::from_str("0123456789012345678901234567890123456789").unwrap();
    let mut spec = PushSpec {
        branches: vec!["main".into(), "develop".into()],
        tag: "1.2.0".into(),
        delete: Some(("release/1.2.0".into(), oid)),
    };
    assert_eq!(
        push_args(&spec),
        [
            "push",
            "--atomic",
            "--porcelain",
            &format!("--force-with-lease=refs/heads/release/1.2.0:{oid}"),
            "origin",
            "refs/heads/main",
            "refs/heads/develop",
            "refs/tags/1.2.0",
            ":refs/heads/release/1.2.0"
        ]
    );
    spec.delete = None;
    assert_eq!(
        push_args(&spec),
        ["push", "--atomic", "--porcelain", "origin", "refs/heads/main", "refs/heads/develop", "refs/tags/1.2.0"]
    );
}

#[test]
fn push_sends_branches_tag_and_deletes_remote_branch() {
    let (t, cfg, bare) = with_origin();
    let prod = cfg.production.clone();
    release_on_origin(&t, &cfg);
    let spec = spec_for(&t, &cfg);
    assert!(spec.delete.is_some());
    push(&git_bin(), t.path(), &spec).unwrap();
    assert_eq!(origin_ref(&bare, &format!("refs/heads/{prod}")), Some(t.tip(&prod)));
    assert_eq!(origin_ref(&bare, "refs/heads/develop"), Some(t.tip("develop")));
    assert!(origin_ref(&bare, "refs/tags/1.2.0").is_some());
    assert_eq!(origin_ref(&bare, "refs/heads/release/1.2.0"), None);
}

#[test]
fn push_deletes_remote_branch_when_local_is_ahead() {
    let (t, cfg, bare) = with_origin();
    let prod = cfg.production.clone();
    release_on_origin(&t, &cfg);
    t.checkout("release/1.2.0");
    t.commit_file("r2.txt", b"more\n", "unpushed release work");
    t.checkout("develop");
    let spec = spec_for(&t, &cfg);
    push(&git_bin(), t.path(), &spec).unwrap();
    assert_eq!(origin_ref(&bare, "refs/heads/release/1.2.0"), None);
    assert_eq!(origin_ref(&bare, &format!("refs/heads/{prod}")), Some(t.tip(&prod)));
}

#[test]
fn rejected_push_changes_nothing_on_origin() {
    let (t, cfg, bare) = with_origin();
    let prod = cfg.production.clone();
    release_on_origin(&t, &cfg);
    let c = clone_of(&bare);
    git(c.path(), &["checkout", &prod]);
    std::fs::write(c.path().join("other.txt"), "x\n").unwrap();
    git(c.path(), &["add", "other.txt"]);
    git(c.path(), &["commit", "-m", "moved on"]);
    git(c.path(), &["push", "origin", &prod]);

    let spec = spec_for(&t, &cfg);
    let refs = |b: &tempfile::TempDir| {
        (
            origin_ref(b, &format!("refs/heads/{prod}")),
            origin_ref(b, "refs/heads/develop"),
            origin_ref(b, "refs/heads/release/1.2.0"),
        )
    };
    let before = refs(&bare);
    let err = push(&git_bin(), t.path(), &spec).unwrap_err();
    assert!(!err.is_empty());
    assert!(err.starts_with(&prod), "{err}");
    assert!(err.contains("[rejected]"), "{err}");
    assert!(!err.contains("error:") && !err.contains("hint:"), "{err}");
    assert_eq!(refs(&bare), before);
    assert_eq!(origin_ref(&bare, "refs/tags/1.2.0"), None);
    assert_eq!(t.head_branch(), "develop");
    assert!(t.repo.find_reference("refs/tags/1.2.0").is_ok());
}

#[test]
fn delete_refused_when_origin_branch_moved() {
    let (t, cfg, bare) = with_origin();
    release_on_origin(&t, &cfg);
    let c = clone_of(&bare);
    git(c.path(), &["checkout", "release/1.2.0"]);
    std::fs::write(c.path().join("late.txt"), "x\n").unwrap();
    git(c.path(), &["add", "late.txt"]);
    git(c.path(), &["commit", "-m", "late"]);
    git(c.path(), &["push", "origin", "release/1.2.0"]);

    let develop_before = origin_ref(&bare, "refs/heads/develop");
    let spec = spec_for(&t, &cfg);
    let err = push(&git_bin(), t.path(), &spec).unwrap_err();
    assert!(err.contains("release/1.2.0") && err.contains("stale info"), "{err}");
    assert_eq!(origin_ref(&bare, "refs/heads/develop"), develop_before);
    assert_eq!(origin_ref(&bare, "refs/tags/1.2.0"), None);
    assert!(origin_ref(&bare, "refs/heads/release/1.2.0").is_some());
}

#[test]
fn push_without_origin_fails() {
    let (t, cfg) = flow_repo();
    let spec = PushSpec { branches: vec![cfg.production.clone(), "develop".into()], tag: "1.2.0".into(), delete: None };
    let err = push(&git_bin(), t.path(), &spec).unwrap_err();
    assert!(!err.is_empty() && !err.contains("hint:"), "{err}");
}
