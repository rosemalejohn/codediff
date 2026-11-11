mod common;
use codediff::diff::{diff_stats, file_diff, files_in_target};
use codediff::model::*;
use common::TestRepo;

fn hunks(d: &FileDiff) -> &Vec<Hunk> {
    match &d.body {
        DiffBody::Text(h) => h,
        other => panic!("expected text, got {other:?}"),
    }
}

fn l(kind: LineKind, o: Option<u32>, n: Option<u32>, t: &str) -> DiffLine {
    DiffLine { kind, old_no: o, new_no: n, text: t.into() }
}

#[test]
fn unstaged_modify_produces_one_hunk() {
    let t = TestRepo::new();
    t.write("a.txt", b"1\n2\n3\n");
    t.stage("a.txt");
    t.commit("c");
    t.write("a.txt", b"1\nX\n3\n");
    let d = file_diff(&t.repo, &DiffTarget::Unstaged, "a.txt", false).unwrap().unwrap();
    let h = hunks(&d);
    assert_eq!(h.len(), 1);
    assert_eq!(
        h[0].lines,
        vec![
            l(LineKind::Context, Some(1), Some(1), "1"),
            l(LineKind::Removed, Some(2), None, "2"),
            l(LineKind::Added, None, Some(2), "X"),
            l(LineKind::Context, Some(3), Some(3), "3"),
        ]
    );
    assert!(h[0].header.starts_with("@@ -1,3 +1,3 @@"));
    assert_eq!((d.additions, d.deletions), (1, 1));
    assert_eq!(d.status, FileStatus::Modified);
}

#[test]
fn staged_diff_uses_index_not_workdir() {
    let t = TestRepo::new();
    t.write("a.txt", b"a\n");
    t.stage("a.txt");
    t.commit("c");
    t.write("a.txt", b"b\n");
    t.stage("a.txt");
    t.write("a.txt", b"c\n");
    let s = file_diff(&t.repo, &DiffTarget::Staged, "a.txt", false).unwrap().unwrap();
    assert!(hunks(&s)[0].lines.iter().any(|x| x.kind == LineKind::Added && x.text == "b"));
    let u = file_diff(&t.repo, &DiffTarget::Unstaged, "a.txt", false).unwrap().unwrap();
    let ls = &hunks(&u)[0].lines;
    assert!(ls.iter().any(|x| x.kind == LineKind::Removed && x.text == "b"));
    assert!(ls.iter().any(|x| x.kind == LineKind::Added && x.text == "c"));
}

#[test]
fn no_change_is_none() {
    let t = TestRepo::new();
    t.write("a.txt", b"a\n");
    t.stage("a.txt");
    t.commit("c");
    assert!(file_diff(&t.repo, &DiffTarget::Unstaged, "a.txt", false).unwrap().is_none());
    assert!(file_diff(&t.repo, &DiffTarget::Staged, "a.txt", false).unwrap().is_none());
}

#[test]
fn staged_diff_on_unborn_head() {
    let t = TestRepo::new();
    t.write("a.txt", b"a\n");
    t.stage("a.txt");
    let d = file_diff(&t.repo, &DiffTarget::Staged, "a.txt", false).unwrap().unwrap();
    assert_eq!(d.status, FileStatus::Added);
    assert_eq!(d.additions, 1);
}

#[test]
fn untracked_file_is_all_added() {
    let t = TestRepo::new();
    t.write("keep.txt", b"k\n");
    t.stage("keep.txt");
    t.commit("c");
    t.write("new.txt", b"x\ny\n");
    let d = file_diff(&t.repo, &DiffTarget::Unstaged, "new.txt", false).unwrap().unwrap();
    assert_eq!(d.status, FileStatus::Untracked);
    let ls = &hunks(&d)[0].lines;
    assert_eq!(ls.len(), 2);
    assert!(ls.iter().all(|x| x.kind == LineKind::Added));
}

#[test]
fn commit_diff_against_first_parent() {
    let t = TestRepo::new();
    t.write("a.txt", b"1\n");
    t.stage("a.txt");
    t.commit("c1");
    t.write("a.txt", b"2\n");
    t.stage("a.txt");
    let c2 = t.commit("c2");
    let d = file_diff(&t.repo, &DiffTarget::Commit(c2), "a.txt", false).unwrap().unwrap();
    assert_eq!((d.additions, d.deletions), (1, 1));
    let files = files_in_target(&t.repo, &DiffTarget::Commit(c2)).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].status, FileStatus::Modified);
}

#[test]
fn root_commit_diff_against_empty() {
    let t = TestRepo::new();
    t.write("a.txt", b"1\n2\n");
    t.stage("a.txt");
    let c1 = t.commit("c1");
    let d = file_diff(&t.repo, &DiffTarget::Commit(c1), "a.txt", false).unwrap().unwrap();
    assert_eq!(d.status, FileStatus::Added);
    assert_eq!((d.additions, d.deletions), (2, 0));
}

#[test]
fn range_diff_combines_commits() {
    let t = TestRepo::new();
    t.write("a.txt", b"1\n");
    t.stage("a.txt");
    let c1 = t.commit("c1");
    t.write("a.txt", b"2\n");
    t.stage("a.txt");
    t.commit("c2");
    t.write("b.txt", b"b\n");
    t.stage("b.txt");
    let c3 = t.commit("c3");
    let target = DiffTarget::Range { base: c1, head: c3 };
    let files = files_in_target(&t.repo, &target).unwrap();
    let got: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.status)).collect();
    assert_eq!(got, vec![("a.txt", FileStatus::Modified), ("b.txt", FileStatus::Added)]);
    let d = file_diff(&t.repo, &target, "a.txt", false).unwrap().unwrap();
    let ls = &hunks(&d)[0].lines;
    assert!(ls.iter().any(|x| x.kind == LineKind::Removed && x.text == "1"));
    assert!(ls.iter().any(|x| x.kind == LineKind::Added && x.text == "2"));
    assert_eq!(diff_stats(&t.repo, &target).unwrap(), (2, 1));
}

#[test]
fn binary_file_diff_reports_sizes() {
    let t = TestRepo::new();
    t.write("b.bin", &[0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    t.stage("b.bin");
    t.commit("c");
    let mut new = vec![0u8; 20];
    new[5] = 7;
    t.write("b.bin", &new);
    let d = file_diff(&t.repo, &DiffTarget::Unstaged, "b.bin", false).unwrap().unwrap();
    assert_eq!(d.body, DiffBody::Binary { old_size: 10, new_size: 20 });
}

#[test]
fn non_utf8_text_is_lossy_not_panic() {
    let t = TestRepo::new();
    t.write("keep.txt", b"k\n");
    t.stage("keep.txt");
    t.commit("c");
    t.write("l.txt", b"caf\xe9\n");
    let d = file_diff(&t.repo, &DiffTarget::Unstaged, "l.txt", false).unwrap().unwrap();
    let ls = &hunks(&d)[0].lines;
    assert!(ls[0].text.contains("caf"));
}

#[test]
fn large_diff_threshold() {
    let t = TestRepo::new();
    t.write("keep.txt", b"k\n");
    t.stage("keep.txt");
    t.commit("c");
    let body: String = (0..5001).map(|i| format!("{i}\n")).collect();
    t.write("big.txt", body.as_bytes());
    let d = file_diff(&t.repo, &DiffTarget::Unstaged, "big.txt", false).unwrap().unwrap();
    assert_eq!(d.body, DiffBody::TooLarge { lines: 5001 });
    let d = file_diff(&t.repo, &DiffTarget::Unstaged, "big.txt", true).unwrap().unwrap();
    assert!(matches!(d.body, DiffBody::Text(_)));
}

#[test]
fn rename_detected() {
    let t = TestRepo::new();
    let body: String = (0..20).map(|i| format!("line {i}\n")).collect();
    t.write("old.txt", body.as_bytes());
    t.stage("old.txt");
    t.commit("c");
    std::fs::remove_file(t.path().join("old.txt")).unwrap();
    let mut idx = t.repo.index().unwrap();
    idx.remove_path(std::path::Path::new("old.txt")).unwrap();
    idx.write().unwrap();
    t.write("new.txt", body.as_bytes());
    t.stage("new.txt");
    let d = file_diff(&t.repo, &DiffTarget::Staged, "new.txt", false).unwrap().unwrap();
    assert_eq!(d.status, FileStatus::Renamed);
    assert_eq!(d.old_path.as_deref(), Some("old.txt"));
}
