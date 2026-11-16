mod common;
use codediff::branch::{checkout, head_info, list_branches, HeadInfo};
use codediff::CoreError;
use common::TestRepo;

fn repo_with_commit() -> TestRepo {
    let t = TestRepo::new();
    t.write("a.txt", b"one\n");
    t.stage("a.txt");
    t.commit("first");
    t
}

fn current_branch(t: &TestRepo) -> String {
    match head_info(&t.repo).unwrap() {
        HeadInfo::Branch(b) => b,
        HeadInfo::Detached(id) => panic!("detached at {id}"),
    }
}

#[test]
fn head_info_on_unborn_branch() {
    let t = TestRepo::new();
    assert!(matches!(head_info(&t.repo).unwrap(), HeadInfo::Branch(b) if !b.is_empty()));
    assert!(list_branches(&t.repo).unwrap().is_empty());
}

#[test]
fn head_info_detached() {
    let t = repo_with_commit();
    let id = t.repo.head().unwrap().target().unwrap();
    t.repo.set_head_detached(id).unwrap();
    assert_eq!(head_info(&t.repo).unwrap(), HeadInfo::Detached(id.to_string()[..7].to_string()));
}

#[test]
fn lists_local_branches_and_marks_current() {
    let t = repo_with_commit();
    let main = current_branch(&t);
    let head = t.repo.head().unwrap().peel_to_commit().unwrap();
    t.repo.branch("feature", &head, false).unwrap();
    let list = list_branches(&t.repo).unwrap();
    assert_eq!(list.len(), 2);
    let cur: Vec<_> = list.iter().filter(|b| b.current).map(|b| b.name.as_str()).collect();
    assert_eq!(cur, vec![main.as_str()]);
    let f = list.iter().find(|b| b.name == "feature").unwrap();
    assert_eq!((f.refname.as_str(), f.remote, f.summary.as_str()), ("refs/heads/feature", false, "first"));
}

#[test]
fn checkout_switches_branch_and_tree() {
    let t = repo_with_commit();
    let main = current_branch(&t);
    let head = t.repo.head().unwrap().peel_to_commit().unwrap();
    t.repo.branch("feature", &head, false).unwrap();
    checkout(&t.repo, "refs/heads/feature").unwrap();
    t.write("a.txt", b"two\n");
    t.stage("a.txt");
    t.commit("second");
    assert_eq!(current_branch(&t), "feature");

    checkout(&t.repo, &format!("refs/heads/{main}")).unwrap();
    assert_eq!(current_branch(&t), main);
    assert_eq!(t.read("a.txt"), b"one\n");
}

#[test]
fn checkout_refuses_to_overwrite_local_changes() {
    let t = repo_with_commit();
    let main = current_branch(&t);
    let head = t.repo.head().unwrap().peel_to_commit().unwrap();
    t.repo.branch("feature", &head, false).unwrap();
    checkout(&t.repo, "refs/heads/feature").unwrap();
    t.write("a.txt", b"two\n");
    t.stage("a.txt");
    t.commit("second");

    t.write("a.txt", b"dirty\n");
    let err = checkout(&t.repo, &format!("refs/heads/{main}")).unwrap_err();
    assert!(matches!(err, CoreError::CheckoutConflict), "{err:?}");
    assert_eq!(current_branch(&t), "feature");
    assert_eq!(t.read("a.txt"), b"dirty\n");
}

#[test]
fn checkout_keeps_unrelated_local_changes() {
    let t = repo_with_commit();
    let head = t.repo.head().unwrap().peel_to_commit().unwrap();
    t.repo.branch("feature", &head, false).unwrap();
    t.write("a.txt", b"dirty\n");
    checkout(&t.repo, "refs/heads/feature").unwrap();
    assert_eq!(current_branch(&t), "feature");
    assert_eq!(t.read("a.txt"), b"dirty\n");
}

#[test]
fn remote_branch_creates_tracking_branch() {
    let t = repo_with_commit();
    let head = t.repo.head().unwrap().peel_to_commit().unwrap();
    t.repo.remote("origin", "https://example.invalid/repo.git").unwrap();
    t.repo.reference("refs/remotes/origin/topic", head.id(), false, "test").unwrap();
    let main = current_branch(&t);
    t.repo.reference(&format!("refs/remotes/origin/{main}"), head.id(), false, "test").unwrap();

    // origin/<main> is hidden behind the local branch; origin/topic is listed.
    let list = list_branches(&t.repo).unwrap();
    let remotes: Vec<_> = list.iter().filter(|b| b.remote).map(|b| b.name.as_str()).collect();
    assert_eq!(remotes, vec!["origin/topic"]);

    checkout(&t.repo, "refs/remotes/origin/topic").unwrap();
    assert_eq!(current_branch(&t), "topic");
    let b = t.repo.find_branch("topic", git2::BranchType::Local).unwrap();
    assert_eq!(b.upstream().unwrap().name().unwrap(), Some("origin/topic"));
    // A second checkout of the remote reuses the local branch.
    checkout(&t.repo, &format!("refs/heads/{main}")).unwrap();
    checkout(&t.repo, "refs/remotes/origin/topic").unwrap();
    assert_eq!(current_branch(&t), "topic");
}
