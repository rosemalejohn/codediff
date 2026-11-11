mod common;
use codediff::diff::file_diff;
use codediff::model::*;
use codediff::stage::*;
use codediff::CoreError;
use common::TestRepo;

const F: &str = "f.txt";

fn numbered() -> Vec<u8> {
    (1..=20).map(|i| format!("{i}\n")).collect::<String>().into_bytes()
}

fn with_edits(edits: &[(usize, &str)]) -> Vec<u8> {
    let mut lines: Vec<String> = (1..=20).map(|i| i.to_string()).collect();
    for (n, s) in edits {
        lines[n - 1] = s.to_string();
    }
    lines.iter().map(|l| format!("{l}\n")).collect::<String>().into_bytes()
}

/// `f.txt` committed as 1..=20, then line 2 -> two and line 18 -> eighteen in workdir.
fn two_hunk_file() -> TestRepo {
    let t = TestRepo::new();
    t.write(F, &numbered());
    t.stage(F);
    t.commit("init");
    t.write(F, &with_edits(&[(2, "two"), (18, "eighteen")]));
    t
}

fn headers(t: &TestRepo, target: DiffTarget, path: &str) -> Vec<String> {
    let d = file_diff(&t.repo, &target, path, false).unwrap().expect("diff");
    match d.body {
        DiffBody::Text(h) => h.into_iter().map(|h| h.header).collect(),
        other => panic!("expected text, got {other:?}"),
    }
}

fn is_stale<T: std::fmt::Debug>(r: codediff::Result<T>) -> bool {
    matches!(r, Err(CoreError::Stale))
}

// ---------- file level ----------

#[test]
fn stage_file_moves_change_to_index() {
    let t = two_hunk_file();
    stage_file(&t.repo, F).unwrap();
    assert_eq!(t.index_blob(F).unwrap(), with_edits(&[(2, "two"), (18, "eighteen")]));
    assert!(file_diff(&t.repo, &DiffTarget::Unstaged, F, false).unwrap().is_none());
}

#[test]
fn stage_new_file_adds_it() {
    let t = two_hunk_file();
    t.write("new.txt", b"hello\n");
    stage_file(&t.repo, "new.txt").unwrap();
    assert_eq!(t.index_blob("new.txt").unwrap(), b"hello\n");
}

#[test]
fn stage_deleted_file() {
    let t = two_hunk_file();
    t.remove(F);
    stage_file(&t.repo, F).unwrap();
    assert!(t.index_blob(F).is_none());
    assert!(!t.exists(F));
}

#[test]
fn unstage_file_restores_head_in_index() {
    let t = two_hunk_file();
    stage_file(&t.repo, F).unwrap();
    unstage_file(&t.repo, F).unwrap();
    assert_eq!(t.index_blob(F).unwrap(), numbered());
    assert_eq!(t.read(F), with_edits(&[(2, "two"), (18, "eighteen")]));
}

#[test]
fn unstage_file_on_unborn_repo() {
    let t = TestRepo::new();
    t.write("n.txt", b"new content\n");
    t.stage("n.txt");
    unstage_file(&t.repo, "n.txt").unwrap();
    assert!(t.index_blob("n.txt").is_none());
    assert_eq!(t.read("n.txt"), b"new content\n");
    let st = t.repo.status_file(std::path::Path::new("n.txt")).unwrap();
    assert!(st.contains(git2::Status::WT_NEW));
}

#[test]
fn unstage_file_of_staged_new_file_leaves_it_untracked() {
    let t = two_hunk_file();
    t.write("n.txt", b"x\n");
    t.stage("n.txt");
    unstage_file(&t.repo, "n.txt").unwrap();
    assert!(t.index_blob("n.txt").is_none());
    assert_eq!(t.read("n.txt"), b"x\n");
}

#[test]
fn unstage_file_of_staged_deletion_restores_entry() {
    let t = two_hunk_file();
    t.remove(F);
    stage_file(&t.repo, F).unwrap();
    unstage_file(&t.repo, F).unwrap();
    assert_eq!(t.index_blob(F).unwrap(), numbered());
    assert!(!t.exists(F));
}

/// Staged rename with edits: unstaging the new path also restores the old path in the
/// index, so the whole rename is unstaged. Workdir is never touched.
fn staged_rename() -> TestRepo {
    let t = TestRepo::new();
    t.write("old.txt", &numbered());
    t.stage("old.txt");
    t.commit("init");
    t.remove("old.txt");
    t.write("new.txt", &with_edits(&[(10, "ten")]));
    let mut idx = t.repo.index().unwrap();
    idx.remove_path(std::path::Path::new("old.txt")).unwrap();
    idx.add_path(std::path::Path::new("new.txt")).unwrap();
    idx.write().unwrap();
    t
}

#[test]
fn unstage_file_on_staged_rename_unstages_both_sides() {
    let t = staged_rename();
    let d = file_diff(&t.repo, &DiffTarget::Staged, "new.txt", false).unwrap().unwrap();
    assert_eq!(d.status, FileStatus::Renamed);
    unstage_file(&t.repo, "new.txt").unwrap();
    assert!(t.index_blob("new.txt").is_none());
    assert_eq!(t.index_blob("old.txt").unwrap(), numbered());
    assert_eq!(t.read("new.txt"), with_edits(&[(10, "ten")]));
    assert!(!t.exists("old.txt"));
}

#[test]
fn unstage_hunk_on_staged_rename_is_stale_and_changes_nothing() {
    let t = staged_rename();
    let hs = headers(&t, DiffTarget::Staged, "new.txt");
    let before_new = t.index_blob("new.txt");
    let before_old = t.index_blob("old.txt");
    assert!(is_stale(unstage_hunk(&t.repo, "new.txt", 0, &hs[0])));
    assert_eq!(t.index_blob("new.txt"), before_new);
    assert_eq!(t.index_blob("old.txt"), before_old);
    assert_eq!(t.read("new.txt"), with_edits(&[(10, "ten")]));
}

#[test]
fn discard_file_restores_head_content() {
    let t = two_hunk_file();
    discard_file(&t.repo, F).unwrap();
    assert_eq!(t.read(F), numbered());
}

#[test]
fn discard_file_restores_deleted_file() {
    let t = two_hunk_file();
    t.remove(F);
    discard_file(&t.repo, F).unwrap();
    assert_eq!(t.read(F), numbered());
}

#[test]
fn discard_file_keeps_staged_part() {
    let t = two_hunk_file();
    t.write(F, b"A\n");
    t.stage(F);
    t.write(F, b"B\n");
    discard_file(&t.repo, F).unwrap();
    assert_eq!(t.read(F), b"A\n");
    assert_eq!(t.index_blob(F).unwrap(), b"A\n");
}

#[test]
fn discard_file_leaves_other_files_alone() {
    let t = two_hunk_file();
    t.write("g.txt", b"g\n");
    t.stage("g.txt");
    t.commit("g");
    t.write("g.txt", b"g changed\n");
    discard_file(&t.repo, F).unwrap();
    assert_eq!(t.read("g.txt"), b"g changed\n");
}

#[test]
fn discard_untracked_deletes_it() {
    let t = two_hunk_file();
    t.write("u.txt", b"untracked\n");
    discard_file(&t.repo, "u.txt").unwrap();
    assert!(!t.exists("u.txt"));
    assert_eq!(t.read(F), with_edits(&[(2, "two"), (18, "eighteen")]));
}

// ---------- hunk level ----------

#[test]
fn two_hunk_fixture_has_two_hunks() {
    let t = two_hunk_file();
    assert_eq!(headers(&t, DiffTarget::Unstaged, F).len(), 2);
}

#[test]
fn stage_hunk_stages_only_that_hunk() {
    let t = two_hunk_file();
    let hs = headers(&t, DiffTarget::Unstaged, F);
    stage_hunk(&t.repo, F, 1, &hs[1]).unwrap();
    assert_eq!(t.index_blob(F).unwrap(), with_edits(&[(18, "eighteen")]));
    assert_eq!(t.read(F), with_edits(&[(2, "two"), (18, "eighteen")]));
}

#[test]
fn stage_hunk_zero_stages_only_first() {
    let t = two_hunk_file();
    let hs = headers(&t, DiffTarget::Unstaged, F);
    stage_hunk(&t.repo, F, 0, &hs[0]).unwrap();
    assert_eq!(t.index_blob(F).unwrap(), with_edits(&[(2, "two")]));
    assert_eq!(t.read(F), with_edits(&[(2, "two"), (18, "eighteen")]));
}

#[test]
fn unstage_hunk_unstages_only_that_hunk() {
    let t = two_hunk_file();
    stage_file(&t.repo, F).unwrap();
    let hs = headers(&t, DiffTarget::Staged, F);
    assert_eq!(hs.len(), 2);
    unstage_hunk(&t.repo, F, 0, &hs[0]).unwrap();
    assert_eq!(t.index_blob(F).unwrap(), with_edits(&[(18, "eighteen")]));
    assert_eq!(t.read(F), with_edits(&[(2, "two"), (18, "eighteen")]));
}

#[test]
fn unstage_hunk_of_staged_new_file_removes_it_from_index() {
    let t = two_hunk_file();
    t.write("n.txt", b"a\nb\n");
    t.stage("n.txt");
    let hs = headers(&t, DiffTarget::Staged, "n.txt");
    unstage_hunk(&t.repo, "n.txt", 0, &hs[0]).unwrap();
    assert!(t.index_blob("n.txt").is_none());
    assert_eq!(t.read("n.txt"), b"a\nb\n");
}

#[test]
fn discard_hunk_discards_only_that_hunk() {
    let t = two_hunk_file();
    let hs = headers(&t, DiffTarget::Unstaged, F);
    discard_hunk(&t.repo, F, 0, &hs[0]).unwrap();
    assert_eq!(t.read(F), with_edits(&[(18, "eighteen")]));
    assert_eq!(t.index_blob(F).unwrap(), numbered());
}

#[test]
fn discard_hunk_keeps_staged_changes() {
    let t = two_hunk_file();
    let hs = headers(&t, DiffTarget::Unstaged, F);
    stage_hunk(&t.repo, F, 0, &hs[0]).unwrap();
    let hs = headers(&t, DiffTarget::Unstaged, F);
    assert_eq!(hs.len(), 1);
    discard_hunk(&t.repo, F, 0, &hs[0]).unwrap();
    assert_eq!(t.read(F), with_edits(&[(2, "two")]));
    assert_eq!(t.index_blob(F).unwrap(), with_edits(&[(2, "two")]));
}

#[test]
fn stage_hunk_refuses_stale_header() {
    let t = two_hunk_file();
    let idx_before = t.index_blob(F);
    let wd_before = t.read(F);
    assert!(is_stale(stage_hunk(&t.repo, F, 0, "@@ -1,1 +1,1 @@")));
    assert_eq!(t.index_blob(F), idx_before);
    assert_eq!(t.read(F), wd_before);
}

#[test]
fn stage_hunk_out_of_range_is_stale() {
    let t = two_hunk_file();
    let hs = headers(&t, DiffTarget::Unstaged, F);
    let idx_before = t.index_blob(F);
    assert!(is_stale(stage_hunk(&t.repo, F, 5, &hs[0])));
    assert_eq!(t.index_blob(F), idx_before);
}

#[test]
fn hunk_ops_on_unchanged_file_are_stale() {
    let t = two_hunk_file();
    discard_file(&t.repo, F).unwrap();
    assert!(is_stale(stage_hunk(&t.repo, F, 0, "@@ -1,5 +1,5 @@")));
    assert!(is_stale(discard_hunk(&t.repo, F, 0, "@@ -1,5 +1,5 @@")));
    assert!(is_stale(unstage_hunk(&t.repo, F, 0, "@@ -1,5 +1,5 @@")));
    assert_eq!(t.read(F), numbered());
}

#[test]
fn discard_hunk_refuses_when_file_changed_since_viewed() {
    let t = two_hunk_file();
    let hs = headers(&t, DiffTarget::Unstaged, F);
    // File edited after the diff was shown: an extra line shifts hunk 0.
    let changed = with_edits(&[(1, "1\nextra"), (2, "two"), (18, "eighteen")]);
    t.write(F, &changed);
    assert!(is_stale(discard_hunk(&t.repo, F, 0, &hs[0])));
    assert_eq!(t.read(F), changed);
    assert_eq!(t.index_blob(F).unwrap(), numbered());
}

#[test]
fn unstage_hunk_refuses_stale_header() {
    let t = two_hunk_file();
    stage_file(&t.repo, F).unwrap();
    let before = t.index_blob(F);
    assert!(is_stale(unstage_hunk(&t.repo, F, 0, "@@ -1,1 +1,1 @@")));
    assert!(is_stale(unstage_hunk(&t.repo, F, 9, "@@ -1,1 +1,1 @@")));
    assert_eq!(t.index_blob(F), before);
}

/// 20 CRLF lines (a..s then `last`) with no final newline.
fn crlf_lines(edits: &[(usize, &str)]) -> Vec<u8> {
    let mut lines: Vec<String> = ('a'..='s').map(|c| c.to_string()).collect();
    lines.push("last".into());
    assert_eq!(lines.len(), 20);
    for (n, s) in edits {
        lines[n - 1] = s.to_string();
    }
    lines.join("\r\n").into_bytes()
}

fn crlf_repo() -> TestRepo {
    let t = TestRepo::new();
    t.repo.config().unwrap().set_bool("core.autocrlf", false).unwrap();
    t.write(F, &crlf_lines(&[]));
    t.stage(F);
    t.commit("crlf");
    t.write(F, &crlf_lines(&[(2, "B"), (19, "S")]));
    t
}

#[test]
fn hunk_ops_preserve_crlf_and_missing_newline() {
    // discard hunk 0
    let t = crlf_repo();
    let hs = headers(&t, DiffTarget::Unstaged, F);
    assert_eq!(hs.len(), 2);
    discard_hunk(&t.repo, F, 0, &hs[0]).unwrap();
    assert_eq!(t.read(F), crlf_lines(&[(19, "S")]));
    assert_eq!(t.index_blob(F).unwrap(), crlf_lines(&[]));

    // discard hunk 1 (touches the line before the unterminated last line)
    let t = crlf_repo();
    let hs = headers(&t, DiffTarget::Unstaged, F);
    discard_hunk(&t.repo, F, 1, &hs[1]).unwrap();
    assert_eq!(t.read(F), crlf_lines(&[(2, "B")]));

    // stage hunk 1, then hunk 0
    let t = crlf_repo();
    let hs = headers(&t, DiffTarget::Unstaged, F);
    stage_hunk(&t.repo, F, 1, &hs[1]).unwrap();
    assert_eq!(t.index_blob(F).unwrap(), crlf_lines(&[(19, "S")]));
    assert_eq!(t.read(F), crlf_lines(&[(2, "B"), (19, "S")]));
    let hs = headers(&t, DiffTarget::Unstaged, F);
    stage_hunk(&t.repo, F, 0, &hs[0]).unwrap();
    assert_eq!(t.index_blob(F).unwrap(), crlf_lines(&[(2, "B"), (19, "S")]));

    // unstage hunk 1 back out
    let hs = headers(&t, DiffTarget::Staged, F);
    unstage_hunk(&t.repo, F, 1, &hs[1]).unwrap();
    assert_eq!(t.index_blob(F).unwrap(), crlf_lines(&[(2, "B")]));
    assert_eq!(t.read(F), crlf_lines(&[(2, "B"), (19, "S")]));
}

#[test]
fn stage_untracked_via_hunk_stages_whole_file() {
    let t = two_hunk_file();
    t.write("u.txt", b"one\ntwo\n");
    let hs = headers(&t, DiffTarget::Unstaged, "u.txt");
    assert_eq!(hs.len(), 1);
    stage_hunk(&t.repo, "u.txt", 0, &hs[0]).unwrap();
    assert_eq!(t.index_blob("u.txt").unwrap(), b"one\ntwo\n");
}

#[test]
fn stage_untracked_via_hunk_checks_header() {
    let t = two_hunk_file();
    t.write("u.txt", b"one\ntwo\n");
    assert!(is_stale(stage_hunk(&t.repo, "u.txt", 0, "@@ -0,0 +1,5 @@")));
    assert!(t.index_blob("u.txt").is_none());
}

#[test]
fn discard_untracked_via_hunk_deletes_it() {
    let t = two_hunk_file();
    t.write("u.txt", b"one\ntwo\n");
    let hs = headers(&t, DiffTarget::Unstaged, "u.txt");
    assert!(is_stale(discard_hunk(&t.repo, "u.txt", 0, "@@ -0,0 +1,9 @@")));
    assert!(t.exists("u.txt"));
    discard_hunk(&t.repo, "u.txt", 0, &hs[0]).unwrap();
    assert!(!t.exists("u.txt"));
}

#[test]
fn stage_hunk_of_deleted_file_removes_it_from_index() {
    let t = two_hunk_file();
    t.remove(F);
    let hs = headers(&t, DiffTarget::Unstaged, F);
    stage_hunk(&t.repo, F, 0, &hs[0]).unwrap();
    assert!(t.index_blob(F).is_none());
}

#[test]
fn discard_hunk_of_deleted_file_restores_it() {
    let t = two_hunk_file();
    t.remove(F);
    let hs = headers(&t, DiffTarget::Unstaged, F);
    discard_hunk(&t.repo, F, 0, &hs[0]).unwrap();
    assert_eq!(t.read(F), numbered());
}

#[test]
fn file_ops_reject_paths_outside_repo() {
    let t = two_hunk_file();
    assert!(discard_file(&t.repo, "../x.txt").is_err());
    assert!(discard_file(&t.repo, "/etc/hosts").is_err());
    assert!(stage_file(&t.repo, "../x.txt").is_err());
}

#[test]
fn unstage_file_with_glob_chars_touches_only_that_path() {
    let t = TestRepo::new();
    t.write("a.txt", b"a\n");
    t.write("[ab].txt", b"glob\n");
    t.stage("a.txt");
    t.stage("[ab].txt");
    t.commit("init");
    t.write("a.txt", b"a2\n");
    t.write("[ab].txt", b"glob2\n");
    t.stage("a.txt");
    t.stage("[ab].txt");
    unstage_file(&t.repo, "[ab].txt").unwrap();
    assert_eq!(t.index_blob("[ab].txt").unwrap(), b"glob\n");
    assert_eq!(t.index_blob("a.txt").unwrap(), b"a2\n");
}

#[test]
fn discard_file_with_glob_chars_touches_only_that_path() {
    let t = TestRepo::new();
    t.write("a.txt", b"a\n");
    t.write("[ab].txt", b"glob\n");
    t.stage("a.txt");
    t.stage("[ab].txt");
    t.commit("init");
    t.write("a.txt", b"a2\n");
    t.write("[ab].txt", b"glob2\n");
    discard_file(&t.repo, "[ab].txt").unwrap();
    assert_eq!(t.read("[ab].txt"), b"glob\n");
    assert_eq!(t.read("a.txt"), b"a2\n");
}
