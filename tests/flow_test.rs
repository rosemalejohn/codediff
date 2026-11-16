mod common;
use codediff::flow::{
    flow_state, init, latest_tag, load_config, next_actions, set_push_default, start, start_error, FlowKind, FlowState,
    TagInfo,
};
use codediff::CoreError;
use common::{default_config, flow_repo, TestRepo};
use git2::BranchType;

use FlowKind::{Feature, Hotfix, Release};

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

fn tag(t: &TestRepo, name: &str, target: &str) {
    let obj = t.repo.revparse_single(target).unwrap();
    t.repo.tag_lightweight(name, &obj, false).unwrap();
}

fn add_ref(t: &TestRepo, name: &str, branch: &str) {
    let id = t.tip(branch);
    t.repo.reference(name, id, false, "test").unwrap();
}

#[test]
fn start_feature_branches_from_develop_and_switches() {
    let (t, cfg) = flow_repo();
    start(&t.repo, &cfg, Feature, "login-form").unwrap();
    assert_eq!(t.head_branch(), "feature/login-form");
    assert_eq!(t.tip("feature/login-form"), t.tip("develop"));
}

#[test]
fn start_hotfix_branches_from_production() {
    let (t, cfg) = flow_repo();
    t.commit_file("b.txt", b"two\n", "develop work");
    assert_ne!(t.tip("develop"), t.tip(&cfg.production));
    start(&t.repo, &cfg, Hotfix, "1.2.1").unwrap();
    assert_eq!(t.head_branch(), "hotfix/1.2.1");
    assert_eq!(t.tip("hotfix/1.2.1"), t.tip(&cfg.production));
}

#[test]
fn start_keeps_uncommitted_changes() {
    let (t, cfg) = flow_repo();
    t.write("a.txt", b"edited\n");
    start(&t.repo, &cfg, Feature, "x").unwrap();
    assert_eq!(t.head_branch(), "feature/x");
    assert_eq!(t.read("a.txt"), b"edited\n");
}

#[test]
fn start_refuses_when_changes_would_be_overwritten() {
    let (t, cfg) = flow_repo();
    t.commit_file("a.txt", b"develop version\n", "change a");
    t.checkout(&cfg.production);
    t.write("a.txt", b"local edit\n");
    assert!(matches!(start(&t.repo, &cfg, Feature, "x"), Err(CoreError::CheckoutConflict)));
    assert!(!t.has_branch("feature/x"));
    assert_eq!(t.head_branch(), cfg.production);
}

#[test]
fn start_error_messages() {
    let (t, cfg) = flow_repo();
    t.branch("feature/taken");
    add_ref(&t, "refs/remotes/origin/feature/remote-only", "develop");
    tag(&t, "1.0.0", "develop");
    t.branch("release/1.1.0");
    let e = |k, n: &str| start_error(&t.repo, &cfg, k, n);
    assert_eq!(e(Feature, "").as_deref(), Some("Enter a name."));
    for bad in ["login form", "x/", "/x", "a..b", "a~b"] {
        assert_eq!(e(Feature, bad).as_deref(), Some("Not a valid branch name."), "{bad}");
    }
    assert_eq!(e(Feature, "ok-name"), None);
    assert_eq!(e(Feature, "taken").as_deref(), Some("feature/taken already exists."));
    assert_eq!(e(Feature, "remote-only").as_deref(), Some("feature/remote-only already exists on origin."));
    assert_eq!(e(Release, "1.0.0").as_deref(), Some("Tag 1.0.0 already exists."));
    assert_eq!(e(Release, "1.2.0").as_deref(), Some("A release is already open: release/1.1.0."));
    assert_eq!(e(Hotfix, "1.0.0").as_deref(), Some("Tag 1.0.0 already exists."));
    assert_eq!(e(Hotfix, "1.2.1"), None);
    t.branch("hotfix/1.0.1");
    assert_eq!(e(Hotfix, "1.2.1").as_deref(), Some("A hotfix is already open: hotfix/1.0.1."));
    assert!(matches!(start(&t.repo, &cfg, Feature, "taken"), Err(CoreError::Flow(m)) if m == "feature/taken already exists."));
}

#[test]
fn start_error_tag_uses_version_prefix() {
    let (t, mut cfg) = flow_repo();
    cfg.version_tag = "v".into();
    tag(&t, "v1.0.0", "develop");
    assert_eq!(start_error(&t.repo, &cfg, Release, "1.0.0").as_deref(), Some("Tag v1.0.0 already exists."));
    assert_eq!(start_error(&t.repo, &cfg, Release, "1.1.0"), None);
}

/// Repository with commits at fixed times 1000 and 2000 on production, then flow initialized.
fn timed_flow_repo(version_tag: &str) -> (TestRepo, codediff::flow::FlowConfig) {
    let t = TestRepo::new();
    t.write("a.txt", b"one\n");
    t.stage("a.txt");
    t.commit_at("c1", 1000);
    t.write("a.txt", b"two\n");
    t.stage("a.txt");
    t.commit_at("c2", 2000);
    let mut cfg = default_config(&t.head_branch());
    cfg.version_tag = version_tag.into();
    (t, cfg)
}

#[test]
fn latest_tag_is_newest_reachable_from_production() {
    let (t, cfg) = timed_flow_repo("");
    tag(&t, "1.1.0", "HEAD");
    tag(&t, "1.0.0", "HEAD~1");
    init(&t.repo, &cfg).unwrap();
    t.write("b.txt", b"x\n");
    t.stage("b.txt");
    t.commit_at("develop only", 3000);
    tag(&t, "9.9.9", "HEAD");
    assert_eq!(latest_tag(&t.repo, &cfg).unwrap(), Some(TagInfo { name: "1.1.0".into(), time: 2000 }));
    // Two tags on the same commit: the higher version wins, compared number by number.
    t.checkout(&cfg.production);
    t.write("c.txt", b"y\n");
    t.stage("c.txt");
    t.commit_at("c4", 4000);
    tag(&t, "1.2.0", "HEAD");
    tag(&t, "1.10.0", "HEAD");
    assert_eq!(latest_tag(&t.repo, &cfg).unwrap().unwrap().name, "1.10.0");
}

#[test]
fn latest_tag_respects_prefix() {
    let (t, cfg) = timed_flow_repo("v");
    tag(&t, "v1.0.0", "HEAD~1");
    tag(&t, "nightly", "HEAD");
    init(&t.repo, &cfg).unwrap();
    assert_eq!(latest_tag(&t.repo, &cfg).unwrap().unwrap().name, "v1.0.0");
}

#[test]
fn latest_tag_none_without_tags() {
    let (t, cfg) = flow_repo();
    assert_eq!(latest_tag(&t.repo, &cfg).unwrap(), None);
}

#[test]
fn flow_state_fills_latest_tag_only_when_initialized() {
    let t = repo_with_commit();
    tag(&t, "1.0.0", "HEAD");
    assert_eq!(flow_state(&t.repo, true).unwrap().latest_tag, None);
    init(&t.repo, &default_config(&t.head_branch())).unwrap();
    assert_eq!(flow_state(&t.repo, true).unwrap().latest_tag.unwrap().name, "1.0.0");
}

fn state(current: Option<(FlowKind, &str)>, features: &[&str], releases: &[&str], hotfixes: &[&str]) -> FlowState {
    let cfg = default_config("main");
    let own = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    FlowState {
        config: Some(cfg.clone()),
        draft: cfg,
        init_blocked: None,
        branches: Vec::new(),
        features: own(features),
        releases: own(releases),
        hotfixes: own(hotfixes),
        current: current.map(|(k, b)| (k, b.to_string())),
        latest_tag: None,
        push_blocked: None,
        push_default: false,
    }
}

fn keys(s: &FlowState) -> Vec<String> {
    next_actions(s).into_iter().map(|a| a.key).collect()
}

#[test]
fn next_actions_by_context() {
    let mut uninitialized = state(None, &[], &[], &[]);
    uninitialized.config = None;
    assert_eq!(keys(&uninitialized), ["init"]);
    let init_action = &next_actions(&uninitialized)[0];
    assert_eq!(init_action.name, "Initialize repository");
    assert_eq!(init_action.detail, "Set up Git Flow branches and prefixes");

    let on_feature = state(Some((Feature, "feature/login-form")), &["feature/login-form"], &[], &[]);
    assert_eq!(
        keys(&on_feature),
        ["finish:feature:feature/login-form", "start:feature", "start:release", "start:hotfix"]
    );
    let first = &next_actions(&on_feature)[0];
    assert_eq!(first.name, "Finish feature login-form");
    assert_eq!(first.detail, "Merge into develop \u{b7} current branch");

    let on_develop = state(None, &["feature/a"], &["release/1.2.0"], &[]);
    assert_eq!(keys(&on_develop), ["finish:release:release/1.2.0", "finish:feature", "start:feature", "start:hotfix"]);
    let rows = next_actions(&on_develop);
    assert_eq!(rows[0].name, "Finish release 1.2.0");
    assert_eq!(rows[0].detail, "Merge into main and develop, tag");
    assert_eq!(rows[1].name, "Finish feature\u{2026}");
    assert_eq!(rows[1].detail, "Pick a feature to merge into develop");
    assert_eq!(rows[2].detail, "Branch from develop");
    assert_eq!(rows[3].name, "Start new hotfix");
    assert_eq!(rows[3].detail, "Branch from main");
}

#[test]
fn next_actions_current_release_and_open_hotfix() {
    let s = state(Some((Release, "release/2.0.0")), &[], &["release/2.0.0"], &["hotfix/1.0.1"]);
    assert_eq!(keys(&s), ["finish:release:release/2.0.0", "finish:hotfix:hotfix/1.0.1", "start:feature"]);
    let rows = next_actions(&s);
    assert_eq!(rows[0].detail, "Merge into main and develop, tag \u{b7} current branch");
    assert_eq!(rows[1].name, "Finish hotfix 1.0.1");
    assert_eq!(rows[1].detail, "Merge into main and develop, tag");
}
