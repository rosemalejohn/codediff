mod common;
use codediff::model::*;
use codediff::status::list_status;
use common::*;

fn e(path: &str, status: FileStatus) -> FileEntry {
    FileEntry { path: path.into(), old_path: None, status }
}

#[test]
fn status_lists_modified_added_deleted_untracked() {
    let t = TestRepo::new();
    t.write("a.txt", b"1\n");
    t.write("b.txt", b"1\n");
    t.stage("a.txt");
    t.stage("b.txt");
    t.commit("init");
    t.write("a.txt", b"2\n");
    std::fs::remove_file(t.path().join("b.txt")).unwrap();
    t.write("c.txt", b"x\n");
    t.write("d.txt", b"x\n");
    t.stage("d.txt");
    let s = list_status(&t.repo).unwrap();
    assert_eq!(
        s.unstaged,
        vec![
            e("a.txt", FileStatus::Modified),
            e("b.txt", FileStatus::Deleted),
            e("c.txt", FileStatus::Untracked)
        ]
    );
    assert_eq!(s.staged, vec![e("d.txt", FileStatus::Added)]);
}

#[test]
fn partially_staged_file_in_both_lists() {
    let t = TestRepo::new();
    t.write("a.txt", b"1\n");
    t.stage("a.txt");
    t.commit("init");
    t.write("a.txt", b"2\n");
    t.stage("a.txt");
    t.write("a.txt", b"3\n");
    let s = list_status(&t.repo).unwrap();
    assert_eq!(s.staged, vec![e("a.txt", FileStatus::Modified)]);
    assert_eq!(s.unstaged, vec![e("a.txt", FileStatus::Modified)]);
}

#[test]
fn status_on_unborn_repo() {
    let t = TestRepo::new();
    t.write("x.txt", b"x\n");
    t.stage("x.txt");
    t.write("y.txt", b"y\n");
    let s = list_status(&t.repo).unwrap();
    assert_eq!(s.staged, vec![e("x.txt", FileStatus::Added)]);
    assert_eq!(s.unstaged, vec![e("y.txt", FileStatus::Untracked)]);
}

#[test]
fn ignored_files_not_listed() {
    let t = TestRepo::new();
    t.write(".gitignore", b"node_modules/\n");
    t.write("node_modules/a.js", b"x\n");
    let s = list_status(&t.repo).unwrap();
    assert!(s.staged.iter().chain(&s.unstaged).all(|f| !f.path.contains("node_modules")));
}

#[test]
fn letter_for_untracked_is_a() {
    assert_eq!(FileStatus::Untracked.letter(), 'A');
}
