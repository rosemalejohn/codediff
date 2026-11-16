mod common;
use codediff::flow::{FlowConfig, FlowKind};
use codediff::flow_finish::{finish, plan_finish, Blocker, FinishPlan, FinishRequest, PushSpec};
use codediff::CoreError;
use codediff::status::list_status;
use common::{feature_req, flow_repo, release_req, TestRepo};
use git2::{Oid, Repository, WorktreeAddOptions};

fn texts(p: &FinishPlan) -> Vec<String> {
    p.steps.iter().map(|s| s.text.clone()).collect()
}

/// HEAD's branch, every reference with its target, and the count of status entries.
fn snapshot(t: &TestRepo) -> (String, Vec<(String, Oid)>, usize) {
    let mut refs: Vec<(String, Oid)> = t
        .repo
        .references()
        .unwrap()
        .flatten()
        .filter_map(|r| Some((r.name().ok()?.to_string(), r.target()?)))
        .collect();
    refs.sort();
    let status = list_status(&t.repo).unwrap();
    (t.head_branch(), refs, status.staged.len() + status.unstaged.len())
}

/// A commit object one step ahead of `parent`, referenced by nothing.
fn loose_commit(repo: &Repository, parent: Oid, msg: &str) -> Oid {
    let parent = repo.find_commit(parent).unwrap();
    let sig = repo.signature().unwrap();
    repo.commit(None, &sig, &sig, msg, &parent.tree().unwrap(), &[&parent]).unwrap()
}

fn set_ref(repo: &Repository, name: &str, target: Oid) {
    repo.reference(name, target, true, "test").unwrap();
}

/// flow_repo with `branch` started from develop and checked out.
fn on_branch(branch: &str) -> (TestRepo, FlowConfig) {
    let (t, cfg) = flow_repo();
    t.branch(branch);
    t.checkout(branch);
    (t, cfg)
}

/// flow_repo with release/1.2.0 one commit ahead of develop, checked out.
fn release_repo() -> (TestRepo, FlowConfig) {
    let (t, cfg) = on_branch("release/1.2.0");
    t.commit_file("version.txt", b"1.2.0\n", "bump version");
    (t, cfg)
}

#[test]
fn feature_plan_steps() {
    let (t, cfg) = on_branch("feature/login-form");
    t.commit_file("login.txt", b"form\n", "one");
    t.commit_file("login.txt", b"form\nfields\n", "two");
    t.commit_file("style.txt", b"css\n", "three");

    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/login-form")).unwrap();
    assert_eq!(p.blocker, None);
    assert_eq!(texts(&p), ["Merge feature/login-form into develop", "Delete feature/login-form", "Switch to develop"]);
    assert_eq!(p.steps[0].note, "3 commits");
    assert!(p.steps.iter().skip(1).all(|s| s.note.is_empty()));
    assert!(p.steps.iter().all(|s| !s.remote));

    let mut req = feature_req("feature/login-form");
    req.delete_branch = false;
    let p2 = plan_finish(&t.repo, &cfg, &req).unwrap();
    assert_eq!(texts(&p2), ["Merge feature/login-form into develop", "Switch to develop"]);
}

#[test]
fn one_commit_note_is_singular() {
    let (t, cfg) = on_branch("feature/x");
    t.commit_file("x.txt", b"x\n", "one");
    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(p.steps[0].note, "1 commit");
}

#[test]
fn already_merged_note() {
    let (t, cfg) = on_branch("feature/x");
    let tip = t.commit_file("x.txt", b"x\n", "one");
    set_ref(&t.repo, "refs/heads/develop", tip);

    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(p.blocker, None);
    assert_eq!(p.steps[0].note, "already merged");
}

#[test]
fn release_plan_steps_with_and_without_push() {
    let (t, cfg) = release_repo();
    let prod = cfg.production.clone();

    let local = plan_finish(&t.repo, &cfg, &release_req("release/1.2.0", "1.2.0", "Release 1.2.0")).unwrap();
    assert_eq!(local.blocker, None);
    assert_eq!(
        texts(&local),
        [
            format!("Merge release/1.2.0 into {prod}"),
            format!("Tag {prod} as 1.2.0"),
            "Merge release/1.2.0 back into develop".into(),
            "Delete release/1.2.0".into(),
            "Switch to develop".into(),
        ]
    );
    assert_eq!(local.steps[0].note, "1 commit");
    assert!(local.steps.iter().all(|s| !s.remote));

    let mut req = release_req("release/1.2.0", "1.2.0", "Release 1.2.0");
    req.push = true;
    let not_on_origin = plan_finish(&t.repo, &cfg, &req).unwrap();
    assert_eq!(not_on_origin.steps.len(), 6);
    assert_eq!(texts(&not_on_origin)[5], format!("Push {prod}, develop and tag 1.2.0 to origin"));

    set_ref(&t.repo, "refs/remotes/origin/release/1.2.0", t.tip("release/1.2.0"));
    let pushed = plan_finish(&t.repo, &cfg, &req).unwrap();
    assert_eq!(pushed.blocker, None);
    assert_eq!(pushed.steps.len(), 7);
    assert_eq!(texts(&pushed)[5], format!("Push {prod}, develop and tag 1.2.0 to origin"));
    assert_eq!(texts(&pushed)[6], "Delete release/1.2.0 on origin");
    assert!(pushed.steps[5].remote && pushed.steps[6].remote);
    assert!(pushed.steps[..5].iter().all(|s| !s.remote));

    req.delete_branch = false;
    let kept = plan_finish(&t.repo, &cfg, &req).unwrap();
    assert_eq!(
        texts(&kept),
        [
            format!("Merge release/1.2.0 into {prod}"),
            format!("Tag {prod} as 1.2.0"),
            "Merge release/1.2.0 back into develop".into(),
            "Switch to develop".into(),
            format!("Push {prod}, develop and tag 1.2.0 to origin"),
        ]
    );
}

#[test]
fn dirty_tree_blocks() {
    let (t, cfg) = flow_repo();
    t.commit_file("b.txt", b"b\n", "add b");
    t.branch("feature/x");
    t.checkout("feature/x");
    t.write("a.txt", b"edited\n");
    t.write("b.txt", b"edited\n");
    t.stage("b.txt");
    t.write("c.txt", b"untracked\n");

    let before = snapshot(&t);
    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(p.blocker, Some(Blocker::Dirty(2)));
    assert_eq!(snapshot(&t), before);
    assert_eq!(Blocker::Dirty(13).title(), "13 uncommitted changes");
    assert_eq!(Blocker::Dirty(1).title(), "1 uncommitted change");
    assert_eq!(
        Blocker::Dirty(2).body(),
        "Finish needs a clean working tree. Commit them, or discard them in Changes, then try again."
    );
}

#[test]
fn untracked_files_alone_do_not_block() {
    let (t, cfg) = on_branch("feature/x");
    t.commit_file("x.txt", b"x\n", "one");
    t.write("scratch.txt", b"untracked\n");

    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(p.blocker, None);
}

#[test]
fn busy_repo_blocks() {
    let (t, cfg) = on_branch("feature/x");
    let tip = t.commit_file("x.txt", b"x\n", "one");
    std::fs::write(t.repo.path().join("MERGE_HEAD"), format!("{tip}\n")).unwrap();

    let before = snapshot(&t);
    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(p.blocker, Some(Blocker::Busy));
    assert_eq!(snapshot(&t), before);
    assert_eq!(Blocker::Busy.title(), "Repository is busy");
    assert_eq!(Blocker::Busy.body(), "A merge, rebase or cherry-pick is in progress. Finish it first.");
}

#[test]
fn other_worktree_blocks() {
    let (t, cfg) = on_branch("feature/x");
    t.commit_file("x.txt", b"x\n", "one");
    let wt_dir = tempfile::tempdir().unwrap();
    let wt_path = wt_dir.path().join("develop-wt");
    let develop = t.repo.find_reference("refs/heads/develop").unwrap();
    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(&develop));
    t.repo.worktree("develop-wt", &wt_path, Some(&opts)).unwrap();

    let before = snapshot(&t);
    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert!(matches!(p.blocker, Some(Blocker::OtherWorktree { ref branch, .. }) if branch == "develop"));
    if let Some(Blocker::OtherWorktree { path, .. }) = &p.blocker {
        assert_eq!(path.canonicalize().unwrap(), wt_path.canonicalize().unwrap());
    }
    assert_eq!(snapshot(&t), before);

    let b = Blocker::OtherWorktree { branch: "develop".into(), path: "/tmp/wt".into() };
    assert_eq!(b.title(), "develop is checked out in another worktree");
    assert_eq!(b.body(), "/tmp/wt. Switch that worktree to another branch first.");
}

#[test]
fn behind_origin_blocks_for_target_and_finished_branch() {
    let (t, cfg) = on_branch("feature/x");
    t.commit_file("x.txt", b"x\n", "one");

    let ahead = loose_commit(&t.repo, t.tip("develop"), "remote work");
    set_ref(&t.repo, "refs/remotes/origin/develop", ahead);
    let before = snapshot(&t);
    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(p.blocker, Some(Blocker::BehindOrigin { branch: "develop".into() }));
    assert_eq!(snapshot(&t), before);

    set_ref(&t.repo, "refs/remotes/origin/develop", t.tip("develop"));
    let ahead = loose_commit(&t.repo, t.tip("feature/x"), "remote work");
    set_ref(&t.repo, "refs/remotes/origin/feature/x", ahead);
    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(p.blocker, Some(Blocker::BehindOrigin { branch: "feature/x".into() }));

    let b = Blocker::BehindOrigin { branch: "develop".into() };
    assert_eq!(b.title(), "develop is behind origin");
    assert_eq!(b.body(), "Pull develop first, then finish again.");
}

#[test]
fn local_branch_ahead_of_origin_does_not_block() {
    let (t, cfg) = on_branch("feature/x");
    let base = t.tip("feature/x");
    t.commit_file("x.txt", b"x\n", "one");
    set_ref(&t.repo, "refs/remotes/origin/feature/x", base);

    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(p.blocker, None);
}

#[test]
fn tag_exists_or_invalid_blocks() {
    let (t, cfg) = release_repo();
    let head = t.repo.head().unwrap().peel(git2::ObjectType::Commit).unwrap();
    t.repo.tag_lightweight("1.2.0", &head, false).unwrap();

    let before = snapshot(&t);
    let p = plan_finish(&t.repo, &cfg, &release_req("release/1.2.0", "1.2.0", "Release 1.2.0")).unwrap();
    assert_eq!(p.blocker, Some(Blocker::TagExists("1.2.0".into())));
    assert_eq!(snapshot(&t), before);

    let p_bad = plan_finish(&t.repo, &cfg, &release_req("release/1.2.0", "1 2", "Release")).unwrap();
    assert_eq!(p_bad.blocker, Some(Blocker::TagInvalid));

    let p_empty = plan_finish(&t.repo, &cfg, &release_req("release/1.2.0", "", "Release")).unwrap();
    assert_eq!(p_empty.blocker, Some(Blocker::TagInvalid));

    assert_eq!(Blocker::TagExists("1.2.0".into()).title(), "Tag 1.2.0 already exists");
    assert_eq!(Blocker::TagExists("1.2.0".into()).body(), "Pick another tag name.");
    assert_eq!(Blocker::TagInvalid.title(), "Tag name is not valid");
    assert_eq!(Blocker::TagInvalid.body(), "Pick another tag name.");
}

#[test]
fn conflict_blocks_and_lists_files() {
    let (t, cfg) = flow_repo();
    t.commit_file("b.txt", b"b\n", "add b");
    t.branch("feature/x");
    t.checkout("feature/x");
    t.write("b.txt", b"feature b\n");
    t.stage("b.txt");
    t.commit_file("a.txt", b"feature a\n", "feature edits");
    t.checkout("develop");
    t.write("a.txt", b"develop a\n");
    t.stage("a.txt");
    t.commit_file("b.txt", b"develop b\n", "develop edits");
    t.checkout("feature/x");

    let before = snapshot(&t);
    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(
        p.blocker,
        Some(Blocker::Conflict {
            target: "develop".into(),
            branch: "feature/x".into(),
            files: vec!["a.txt".into(), "b.txt".into()],
        })
    );
    assert_eq!(snapshot(&t), before);
    assert!(!t.repo.path().join("MERGE_HEAD").exists());

    let b = p.blocker.unwrap();
    assert_eq!(b.title(), "Cannot merge cleanly");
    assert_eq!(
        b.body(),
        "develop and feature/x both changed these files. Nothing was changed. \
         Merge develop into feature/x first (agent or terminal), then finish again."
    );
}

#[test]
fn release_back_merge_conflict_names_develop() {
    let (t, cfg) = on_branch("release/1.2.0");
    t.commit_file("a.txt", b"release a\n", "release edit");
    t.checkout("develop");
    t.commit_file("a.txt", b"develop a\n", "develop edit");
    t.checkout("release/1.2.0");

    let before = snapshot(&t);
    let p = plan_finish(&t.repo, &cfg, &release_req("release/1.2.0", "1.2.0", "Release 1.2.0")).unwrap();
    assert!(matches!(p.blocker, Some(Blocker::Conflict { ref target, .. }) if target == "develop"));
    assert_eq!(
        p.blocker,
        Some(Blocker::Conflict {
            target: "develop".into(),
            branch: "release/1.2.0".into(),
            files: vec!["a.txt".into()],
        })
    );
    assert_eq!(snapshot(&t), before);
}

#[test]
fn release_conflict_with_production_names_production() {
    let (t, cfg) = on_branch("release/1.2.0");
    t.commit_file("a.txt", b"release a\n", "release edit");
    t.checkout(&cfg.production);
    t.commit_file("a.txt", b"production a\n", "production edit");
    t.checkout("release/1.2.0");

    let p = plan_finish(&t.repo, &cfg, &release_req("release/1.2.0", "1.2.0", "Release 1.2.0")).unwrap();
    assert_eq!(
        p.blocker,
        Some(Blocker::Conflict {
            target: cfg.production.clone(),
            branch: "release/1.2.0".into(),
            files: vec!["a.txt".into()],
        })
    );
}

#[test]
fn untracked_collision_blocks() {
    let (t, cfg) = on_branch("feature/x");
    t.commit_file("new.txt", b"from feature\n", "add new");
    t.checkout("develop");
    t.write("new.txt", b"local scratch\n");

    let before = snapshot(&t);
    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(p.blocker, Some(Blocker::Collision(vec!["new.txt".into()])));
    assert_eq!(snapshot(&t), before);
    assert_eq!(t.read("new.txt"), b"local scratch\n");

    let b = p.blocker.unwrap();
    assert_eq!(b.title(), "Untracked files are in the way");
    assert_eq!(b.body(), "Move or delete them, then try again.");
}

#[test]
fn planning_changes_nothing() {
    let (t, cfg) = release_repo();
    let before = snapshot(&t);
    let p = plan_finish(&t.repo, &cfg, &release_req("release/1.2.0", "1.2.0", "Release 1.2.0")).unwrap();
    assert_eq!(p.blocker, None);
    assert_eq!(snapshot(&t), before);
    assert_eq!(t.read("version.txt"), b"1.2.0\n");
    assert_eq!(t.index_blob("version.txt").unwrap(), b"1.2.0\n");

    let (t, cfg) = flow_repo();
    t.branch("feature/x");
    t.checkout("feature/x");
    t.commit_file("a.txt", b"feature\n", "feature edit");
    t.checkout("develop");
    t.commit_file("a.txt", b"develop\n", "develop edit");
    let before = snapshot(&t);
    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert!(matches!(p.blocker, Some(Blocker::Conflict { .. })));
    assert_eq!(snapshot(&t), before);
    assert_eq!(t.read("a.txt"), b"develop\n");
    assert!(!t.repo.path().join("MERGE_HEAD").exists());
}

#[test]
fn untracked_file_where_a_directory_is_needed_blocks() {
    let (t, cfg) = on_branch("feature/x");
    t.commit_file("docs/guide.txt", b"guide\n", "add docs");
    t.checkout("develop");
    t.write("docs", b"local scratch\n");

    let p = plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(p.blocker, Some(Blocker::Collision(vec!["docs".into()])));
}

fn hotfix_req(branch: &str, tag: &str, message: &str) -> FinishRequest {
    FinishRequest { kind: FlowKind::Hotfix, ..release_req(branch, tag, message) }
}

fn parents(t: &TestRepo, id: Oid) -> Vec<Oid> {
    t.repo.find_commit(id).unwrap().parent_ids().collect()
}

#[test]
fn finish_feature_creates_merge_commit() {
    let (t, cfg) = on_branch("feature/login-form");
    let feature_tip = t.commit_file("feature-file.txt", b"feature\n", "add feature file");
    t.checkout("develop");
    let old_develop = t.commit_file("develop-file.txt", b"develop\n", "develop moves on");
    t.checkout("feature/login-form");

    let out = finish(&t.repo, &cfg, &feature_req("feature/login-form")).unwrap();
    let tip = t.repo.find_commit(t.tip("develop")).unwrap();
    assert_eq!(tip.parent_ids().collect::<Vec<_>>(), [old_develop, feature_tip]);
    assert_eq!(tip.message().unwrap(), "Merge branch 'feature/login-form' into develop");
    assert_eq!(t.head_branch(), "develop");
    assert!(!t.has_branch("feature/login-form"));
    assert!(t.exists("feature-file.txt"));
    assert!(t.exists("develop-file.txt"));
    assert_eq!(out.notice, "Finished feature/login-form. Merged into develop and deleted the branch.");
    assert_eq!(out.pushed_notice, "");
    assert_eq!(out.push, None);
}

#[test]
fn finish_single_commit_feature_fast_forwards() {
    let (t, cfg) = on_branch("feature/x");
    let feature_tip = t.commit_file("feature-file.txt", b"feature\n", "add feature file");

    finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(t.tip("develop"), feature_tip);
    assert_eq!(t.head_branch(), "develop");
    assert!(!t.has_branch("feature/x"));
    assert!(t.exists("feature-file.txt"));
}

#[test]
fn finish_feature_already_merged_just_deletes() {
    let (t, cfg) = on_branch("feature/x");
    let feature_tip = t.commit_file("feature-file.txt", b"feature\n", "add feature file");
    set_ref(&t.repo, "refs/heads/develop", feature_tip);
    let old_develop = t.tip("develop");

    let out = finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(t.tip("develop"), old_develop);
    assert!(!t.has_branch("feature/x"));
    assert_eq!(t.head_branch(), "develop");
    assert_eq!(out.notice, "Finished feature/x. Merged into develop and deleted the branch.");
}

#[test]
fn finish_keeps_branch_when_delete_unticked() {
    let (t, cfg) = on_branch("feature/x");
    let feature_tip = t.commit_file("feature-file.txt", b"feature\n", "add feature file");
    let mut req = feature_req("feature/x");
    req.delete_branch = false;

    let out = finish(&t.repo, &cfg, &req).unwrap();
    assert!(t.has_branch("feature/x"));
    assert_eq!(t.tip("feature/x"), feature_tip);
    assert_eq!(t.head_branch(), "develop");
    assert_eq!(out.notice, "Finished feature/x. Merged into develop.");
    assert_eq!(out.push, None);
}

#[test]
fn finish_feature_while_on_develop_updates_worktree() {
    let (t, cfg) = on_branch("feature/x");
    let old_develop = t.tip("develop");
    t.commit_file("feature-file.txt", b"feature\n", "one");
    let feature_tip = t.commit_file("feature-file.txt", b"feature\nmore\n", "two");
    t.checkout("develop");
    assert!(!t.exists("feature-file.txt"));

    finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap();
    assert_eq!(parents(&t, t.tip("develop")), [old_develop, feature_tip]);
    assert!(t.exists("feature-file.txt"));
    assert_eq!(t.read("feature-file.txt"), b"feature\nmore\n");
    let s = list_status(&t.repo).unwrap();
    assert!(s.staged.is_empty() && s.unstaged.is_empty());
}

#[test]
fn finish_release_graph() {
    let (t, cfg) = release_repo();
    let old_prod = t.tip(&cfg.production);
    let old_develop = t.tip("develop");
    let release_tip = t.tip("release/1.2.0");

    let out = finish(&t.repo, &cfg, &release_req("release/1.2.0", "1.2.0", "Release 1.2.0")).unwrap();
    let prod = t.repo.find_commit(t.tip(&cfg.production)).unwrap();
    assert_eq!(prod.parent_ids().collect::<Vec<_>>(), [old_prod, release_tip]);
    assert_eq!(prod.message().unwrap(), format!("Merge branch 'release/1.2.0' into {}", cfg.production));
    let tag = t.repo.find_reference("refs/tags/1.2.0").unwrap().peel_to_tag().unwrap();
    assert_eq!((tag.target_id(), tag.message().unwrap().unwrap().trim()), (prod.id(), "Release 1.2.0"));
    let dev = t.repo.find_commit(t.tip("develop")).unwrap();
    assert_eq!(dev.parent_ids().collect::<Vec<_>>(), [old_develop, prod.id()]);
    assert_eq!(dev.message().unwrap(), "Merge tag '1.2.0' into develop");
    assert_eq!(t.head_branch(), "develop");
    assert!(!t.has_branch("release/1.2.0"));
    assert_eq!(t.read("version.txt"), b"1.2.0\n");
    assert_eq!(out.notice, "Finished release/1.2.0. Tagged 1.2.0. Not pushed.");
    assert_eq!(out.pushed_notice, "");
    assert_eq!(out.push, None);
}

#[test]
fn finish_hotfix_merges_into_production_and_develop() {
    let (t, cfg) = flow_repo();
    t.checkout(&cfg.production);
    t.branch("hotfix/1.2.1");
    t.checkout("hotfix/1.2.1");
    let hotfix_tip = t.commit_file("fix.txt", b"fix\n", "fix the bug");
    t.checkout("develop");
    let old_develop = t.commit_file("develop-file.txt", b"develop\n", "develop moves on");
    t.checkout("hotfix/1.2.1");
    let old_prod = t.tip(&cfg.production);

    let out = finish(&t.repo, &cfg, &hotfix_req("hotfix/1.2.1", "1.2.1", "Hotfix 1.2.1")).unwrap();
    let prod = t.repo.find_commit(t.tip(&cfg.production)).unwrap();
    assert_eq!(prod.parent_ids().collect::<Vec<_>>(), [old_prod, hotfix_tip]);
    assert_eq!(prod.message().unwrap(), format!("Merge branch 'hotfix/1.2.1' into {}", cfg.production));
    let tag = t.repo.find_reference("refs/tags/1.2.1").unwrap().peel_to_tag().unwrap();
    assert_eq!((tag.target_id(), tag.message().unwrap().unwrap().trim()), (prod.id(), "Hotfix 1.2.1"));
    let dev = t.repo.find_commit(t.tip("develop")).unwrap();
    assert_eq!(dev.parent_ids().collect::<Vec<_>>(), [old_develop, prod.id()]);
    assert_eq!(dev.message().unwrap(), "Merge tag '1.2.1' into develop");
    assert_eq!(t.head_branch(), "develop");
    assert!(!t.has_branch("hotfix/1.2.1"));
    assert!(t.exists("fix.txt") && t.exists("develop-file.txt"));
    let s = list_status(&t.repo).unwrap();
    assert!(s.staged.is_empty() && s.unstaged.is_empty());
    assert_eq!(out.notice, "Finished hotfix/1.2.1. Tagged 1.2.1. Not pushed.");
}

#[test]
fn finish_with_push_returns_spec() {
    let (t, cfg) = release_repo();
    let release_tip = t.tip("release/1.2.0");
    set_ref(&t.repo, "refs/remotes/origin/release/1.2.0", release_tip);
    let mut req = release_req("release/1.2.0", "1.2.0", "Release 1.2.0");
    req.push = true;

    let out = finish(&t.repo, &cfg, &req).unwrap();
    assert_eq!(
        out.push,
        Some(PushSpec {
            branches: vec![cfg.production.clone(), "develop".into()],
            tag: "1.2.0".into(),
            delete: Some(("release/1.2.0".into(), release_tip)),
        })
    );
    assert_eq!(
        out.pushed_notice,
        format!("Finished release/1.2.0. Tagged 1.2.0 and pushed {}, develop and the tag to origin.", cfg.production)
    );
    assert_eq!(out.notice, "Finished release/1.2.0. Tagged 1.2.0. Not pushed.");

    // Not on origin: nothing to delete there.
    let (t, cfg) = release_repo();
    let out = finish(&t.repo, &cfg, &req).unwrap();
    assert_eq!(out.push.unwrap().delete, None);

    // On origin, but the branch is kept.
    let (t, cfg) = release_repo();
    set_ref(&t.repo, "refs/remotes/origin/release/1.2.0", t.tip("release/1.2.0"));
    req.delete_branch = false;
    let out = finish(&t.repo, &cfg, &req).unwrap();
    assert_eq!(out.push.unwrap().delete, None);
    assert!(t.has_branch("release/1.2.0"));
}

#[test]
fn finish_refuses_when_blocked_and_changes_nothing() {
    let (t, cfg) = on_branch("feature/x");
    t.commit_file("feature-file.txt", b"feature\n", "one");
    t.write("a.txt", b"dirty\n");
    let req = feature_req("feature/x");

    let before = snapshot(&t);
    let err = finish(&t.repo, &cfg, &req);
    assert!(matches!(err, Err(CoreError::Flow(ref m)) if m.starts_with("1 uncommitted change")));
    if let Err(CoreError::Flow(m)) = err {
        assert_eq!(
            m,
            "1 uncommitted change. Finish needs a clean working tree. \
             Commit them, or discard them in Changes, then try again."
        );
    }
    assert_eq!(snapshot(&t), before);
    assert_eq!(t.read("a.txt"), b"dirty\n");
}

#[test]
fn finish_conflict_changes_nothing() {
    let (t, cfg) = on_branch("release/1.2.0");
    t.commit_file("a.txt", b"release a\n", "release edit");
    t.checkout("develop");
    t.commit_file("a.txt", b"develop a\n", "develop edit");
    t.checkout("release/1.2.0");

    let before = snapshot(&t);
    let err = finish(&t.repo, &cfg, &release_req("release/1.2.0", "1.2.0", "Release 1.2.0"));
    assert!(matches!(err, Err(CoreError::Flow(ref m)) if m.starts_with("Cannot merge cleanly")));
    assert_eq!(snapshot(&t), before);
    assert!(t.repo.find_reference("refs/tags/1.2.0").is_err());
    assert_eq!(t.read("a.txt"), b"release a\n");
    assert!(!t.repo.path().join("MERGE_HEAD").exists());
}

/// The checkout runs before any reference moves: when it fails for a reason
/// the planner cannot see (here the working directory is read-only), the
/// references, HEAD, the index and the working tree are all unchanged.
#[test]
fn finish_checkout_failure_moves_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let (t, cfg) = on_branch("feature/x");
    t.commit_file("feature-file.txt", b"feature\n", "add feature file");
    t.checkout("develop");
    t.commit_file("develop-file.txt", b"develop\n", "develop moves on");
    t.checkout("feature/x");
    assert_eq!(plan_finish(&t.repo, &cfg, &feature_req("feature/x")).unwrap().blocker, None);

    let before = snapshot(&t);
    let set_mode = |mode| std::fs::set_permissions(t.path(), std::fs::Permissions::from_mode(mode)).unwrap();
    set_mode(0o555);
    let result = finish(&t.repo, &cfg, &feature_req("feature/x"));
    set_mode(0o755);

    assert!(result.is_err());
    assert_eq!(snapshot(&t), before);
    assert_eq!(t.head_branch(), "feature/x");
    assert!(!t.exists("develop-file.txt"));
    assert_eq!(t.index_blob("develop-file.txt"), None);
}

/// A reference move that fails names the step that stopped and what already ran.
#[test]
fn finish_names_the_step_that_stopped() {
    let (t, cfg) = on_branch("feature/x");
    let feature_tip = t.commit_file("feature-file.txt", b"feature\n", "add feature file");
    let lock = t.repo.path().join("refs/heads/feature/x.lock");
    std::fs::write(&lock, b"").unwrap();

    let err = finish(&t.repo, &cfg, &feature_req("feature/x"));
    std::fs::remove_file(&lock).unwrap();

    let Err(CoreError::Flow(m)) = err else { panic!("expected a Flow error, got {err:?}") };
    let (head, cause) = m.split_once(" Cause: ").expect("message names the cause");
    assert_eq!(head, "Finish stopped at: Delete feature/x. Already done: Merge feature/x into develop, Switch to develop.");
    assert!(!cause.trim().is_empty());
    assert_eq!(t.tip("develop"), feature_tip);
    assert_eq!(t.head_branch(), "develop");
    assert!(t.has_branch("feature/x"));
}

/// The remote delete is leased on the commit last seen on origin, not on the
/// local tip, so unpushed commits on the finished branch do not break it.
#[test]
fn finish_push_delete_leases_origin_tip() {
    let (t, cfg) = release_repo();
    let origin_tip = t.tip("release/1.2.0");
    set_ref(&t.repo, "refs/remotes/origin/release/1.2.0", origin_tip);
    let local_tip = t.commit_file("notes.txt", b"unpushed\n", "unpushed commit");
    assert_ne!(origin_tip, local_tip);
    let mut req = release_req("release/1.2.0", "1.2.0", "Release 1.2.0");
    req.push = true;

    let out = finish(&t.repo, &cfg, &req).unwrap();
    assert_eq!(out.push.unwrap().delete, Some(("release/1.2.0".to_string(), origin_tip)));
    assert!(!t.has_branch("release/1.2.0"));
}
