mod common;
use codediff::flow::{flow_state, init, load_config, set_push_default, FlowKind};
use codediff::CoreError;
use common::{default_config, flow_repo, TestRepo};
use git2::BranchType;

fn repo_with_commit() -> TestRepo {
    let t = TestRepo::new();
    t.commit_file("a.txt", b"one\n", "first");
    t
}

fn set_keys(t: &TestRepo, production: &str, develop: &str, version_tag: Option<&str>) {
    let mut c = t.repo.config().unwrap();
    c.set_str("gitflow.branch.master", production).unwrap();
    c.set_str("gitflow.branch.develop", develop).unwrap();
    c.set_str("gitflow.prefix.feature", "feature/").unwrap();
    c.set_str("gitflow.prefix.release", "release/").unwrap();
    c.set_str("gitflow.prefix.hotfix", "hotfix/").unwrap();
    if let Some(v) = version_tag {
        c.set_str("gitflow.prefix.versiontag", v).unwrap();
    }
}

#[test]
fn not_initialized_by_default() {
    let t = repo_with_commit();
    let s = flow_state(&t.repo, true).unwrap();
    assert_eq!(s.config, None);
    assert_eq!(
        (s.draft.develop.as_str(), s.draft.feature.as_str(), s.draft.release.as_str(), s.draft.hotfix.as_str(), s.draft.version_tag.as_str()),
        ("develop", "feature/", "release/", "hotfix/", "")
    );
    assert_eq!(s.draft.production, t.head_branch());
}

#[test]
fn draft_production_prefers_main_then_master() {
    let t = repo_with_commit();
    // The initial branch may already be one of these.
    for name in ["master", "main"] {
        if !t.has_branch(name) {
            t.branch(name);
        }
    }
    t.branch("work");
    t.checkout("work");
    assert_eq!(flow_state(&t.repo, true).unwrap().draft.production, "main");
    t.repo.find_branch("main", BranchType::Local).unwrap().delete().unwrap();
    assert_eq!(flow_state(&t.repo, true).unwrap().draft.production, "master");
}

#[test]
fn init_writes_standard_keys_and_switches_to_develop() {
    let t = repo_with_commit();
    let cfg = default_config(&t.head_branch());
    init(&t.repo, &cfg).unwrap();
    let c = t.repo.config().unwrap();
    assert_eq!(c.get_string("gitflow.branch.master").unwrap(), cfg.production);
    assert_eq!(c.get_string("gitflow.branch.develop").unwrap(), "develop");
    assert_eq!(c.get_string("gitflow.prefix.feature").unwrap(), "feature/");
    assert_eq!(c.get_string("gitflow.prefix.bugfix").unwrap(), "bugfix/");
    assert_eq!(c.get_string("gitflow.prefix.support").unwrap(), "support/");
    assert_eq!(c.get_string("gitflow.prefix.versiontag").unwrap(), "");
    assert_eq!(t.head_branch(), "develop");
    assert_eq!(t.tip("develop"), t.tip(&cfg.production));
    assert_eq!(load_config(&t.repo).unwrap(), Some(cfg));
}

#[test]
fn detects_repo_initialized_elsewhere() {
    let t = repo_with_commit();
    let prod = t.head_branch();
    t.branch("develop");
    set_keys(&t, &prod, "develop", Some("v"));
    let cfg = load_config(&t.repo).unwrap().unwrap();
    assert_eq!(cfg.version_tag, "v");
    assert_eq!(cfg.production, prod);
}

#[test]
fn missing_develop_branch_is_not_initialized_but_prefills() {
    let t = repo_with_commit();
    let prod = t.head_branch();
    set_keys(&t, &prod, "dev", None);
    let s = flow_state(&t.repo, true).unwrap();
    assert_eq!(s.config, None);
    assert_eq!(s.draft.develop, "dev");
}

#[test]
fn init_creates_develop_from_origin_when_present() {
    let t = repo_with_commit();
    let prod = t.head_branch();
    let second = t.commit_file("b.txt", b"two\n", "second");
    t.repo.remote("origin", "https://example.com/x.git").unwrap();
    t.repo.reference("refs/remotes/origin/develop", second, true, "test").unwrap();
    // Move the production branch back so develop cannot come from it by accident.
    let first = t.repo.find_commit(second).unwrap().parent_id(0).unwrap();
    t.repo.reference(&format!("refs/heads/{prod}"), first, true, "test").unwrap();
    t.repo.reset(&t.repo.find_object(first, None).unwrap(), git2::ResetType::Hard, None).unwrap();
    init(&t.repo, &default_config(&prod)).unwrap();
    assert_eq!(t.tip("develop"), second);
    assert_eq!(
        t.repo.find_branch("develop", BranchType::Local).unwrap().upstream().unwrap().name().unwrap(),
        Some("origin/develop")
    );
}

#[test]
fn init_refuses_same_branches() {
    let t = repo_with_commit();
    let prod = t.head_branch();
    let mut same = default_config(&prod);
    same.develop = prod;
    assert!(matches!(init(&t.repo, &same), Err(CoreError::Flow(m)) if m == "Production and development branches must differ."));
    assert_eq!(load_config(&t.repo).unwrap(), None);
}

#[test]
fn init_refuses_missing_production() {
    let t = repo_with_commit();
    let cfg = default_config("nope");
    assert!(matches!(init(&t.repo, &cfg), Err(CoreError::Flow(m)) if m == "Production branch nope does not exist."));
}

#[test]
fn init_refuses_bad_prefix() {
    let t = repo_with_commit();
    let prod = t.head_branch();
    for bad_prefix in ["", "fea ture/"] {
        let mut bad = default_config(&prod);
        bad.feature = bad_prefix.into();
        assert!(matches!(init(&t.repo, &bad), Err(CoreError::Flow(m)) if m == "Feature prefix is not valid."));
    }
    let mut bad = default_config(&prod);
    bad.release = "".into();
    assert!(matches!(init(&t.repo, &bad), Err(CoreError::Flow(m)) if m == "Release prefix is not valid."));
    let mut bad = default_config(&prod);
    bad.hotfix = "".into();
    assert!(matches!(init(&t.repo, &bad), Err(CoreError::Flow(m)) if m == "Hotfix prefix is not valid."));
    assert!(!t.has_branch("develop"));
}

#[test]
fn init_stands_when_checkout_is_blocked() {
    let t = repo_with_commit();
    let prod = t.head_branch();
    t.branch("work");
    t.checkout("work");
    t.commit_file("a.txt", b"work\n", "work change");
    t.write("a.txt", b"dirty\n");
    let cfg = default_config(&prod);
    assert!(matches!(init(&t.repo, &cfg), Err(CoreError::CheckoutConflict)));
    assert!(load_config(&t.repo).unwrap().is_some());
    assert_eq!(t.head_branch(), "work");
}

#[test]
fn unborn_repo_cannot_init() {
    assert_eq!(
        flow_state(&TestRepo::new().repo, true).unwrap().init_blocked.as_deref(),
        Some("Repository has no commits yet.")
    );
    assert_eq!(flow_state(&repo_with_commit().repo, true).unwrap().init_blocked, None);
}

#[test]
fn state_lists_flow_branches_and_current() {
    let (t, cfg) = flow_repo();
    let base = t.repo.head().unwrap().peel_to_commit().unwrap();
    for (name, secs) in [("feature/a", 1_000), ("feature/b", 3_000), ("release/1.0.0", 2_000)] {
        t.repo.branch(name, &base, false).unwrap();
        t.checkout(name);
        t.commit_file(&format!("{}.txt", name.replace('/', "_")), b"x\n", name);
        // Re-stamp the tip with a deterministic time.
        t.repo.reset(base.as_object(), git2::ResetType::Soft, None).unwrap();
        t.commit_at(name, secs);
    }
    let s = flow_state(&t.repo, true).unwrap();
    assert_eq!(s.config, Some(cfg));
    assert_eq!(s.features, ["feature/b", "feature/a"]);
    assert_eq!(s.releases, ["release/1.0.0"]);
    assert!(s.hotfixes.is_empty());
    // Last checked out was release/1.0.0; switch to feature/b.
    t.checkout("feature/b");
    let s = flow_state(&t.repo, true).unwrap();
    assert_eq!(s.current, Some((FlowKind::Feature, "feature/b".into())));
    assert!(s.branches.contains(&"develop".to_string()));
    assert_eq!(s.latest_tag, None);
}

#[test]
fn equal_tip_times_order_by_name() {
    let (t, _) = flow_repo();
    for name in ["feature/z", "feature/m"] {
        t.checkout("develop");
        t.branch(name);
    }
    let s = flow_state(&t.repo, true).unwrap();
    assert_eq!(s.features, ["feature/m", "feature/z"]);
}

#[test]
fn push_availability_and_default() {
    let (t, _) = flow_repo();
    assert_eq!(flow_state(&t.repo, true).unwrap().push_blocked.as_deref(), Some("No origin remote."));
    assert!(!flow_state(&t.repo, true).unwrap().push_default);
    t.repo.remote("origin", "https://example.com/x.git").unwrap();
    assert_eq!(flow_state(&t.repo, false).unwrap().push_blocked.as_deref(), Some("Git command line tools not found."));
    assert_eq!(flow_state(&t.repo, true).unwrap().push_blocked, None);
    set_push_default(&t.repo, true).unwrap();
    assert!(flow_state(&t.repo, true).unwrap().push_default);
    set_push_default(&t.repo, false).unwrap();
    assert!(!flow_state(&t.repo, true).unwrap().push_default);
}
