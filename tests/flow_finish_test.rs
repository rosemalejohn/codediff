mod common;
use codediff::flow::FlowConfig;
use codediff::flow_finish::{plan_finish, Blocker, FinishPlan};
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
