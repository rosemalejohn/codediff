mod common;
use codediff::controller::{Command, Controller, Event, Selection};
use codediff::model::*;
use common::TestRepo;
use git2::Oid;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(2);

struct Harness {
    ctl: Controller,
    events: Arc<Mutex<Vec<Event>>>,
    _cfg: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let cfg = tempfile::tempdir().unwrap();
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let sink = events.clone();
        let ctl = Controller::spawn(cfg.path().join("recent.json"), move |e| {
            sink.lock().unwrap().push(e)
        });
        Harness {
            ctl,
            events,
            _cfg: cfg,
        }
    }

    fn recent_file(&self) -> PathBuf {
        self._cfg.path().join("recent.json")
    }

    fn mark(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    /// First event at index >= `from` matching `pred`, waiting up to 2 s.
    fn wait_for(&self, from: usize, pred: impl Fn(&Event) -> bool) -> (usize, Event) {
        let start = Instant::now();
        loop {
            {
                let ev = self.events.lock().unwrap();
                if let Some(i) = (from..ev.len()).find(|&i| pred(&ev[i])) {
                    return (i, ev[i].clone());
                }
            }
            if start.elapsed() > TIMEOUT {
                panic!("timed out; events: {:#?}", self.events.lock().unwrap());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Opens `path` and waits for the trailing `Diff(None)` of the open sequence.
    fn open(&self, path: &std::path::Path) -> usize {
        let m = self.mark();
        self.ctl.send(Command::OpenRepo(path.to_path_buf()));
        self.wait_for(m, |e| matches!(e, Event::Diff(None))).0 + 1
    }
}

fn status_has(e: &Event, staged: &[&str], unstaged: &[&str]) -> bool {
    let Event::Status(s) = e else { return false };
    let names = |v: &Vec<FileEntry>| v.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
    names(&s.staged) == staged && names(&s.unstaged) == unstaged
}

fn numbered() -> Vec<u8> {
    (1..=20)
        .map(|i| format!("{i}\n"))
        .collect::<String>()
        .into_bytes()
}

/// `f.txt` committed as 1..=20, then lines 2 and 18 edited in the workdir.
fn two_hunk_repo() -> TestRepo {
    let t = TestRepo::new();
    t.write("f.txt", &numbered());
    t.stage("f.txt");
    t.commit("init");
    let mut s = String::from_utf8(numbered()).unwrap();
    s = s
        .replacen("2\n", "two\n", 1)
        .replacen("18\n", "eighteen\n", 1);
    t.write("f.txt", s.as_bytes());
    t
}

fn select_unstaged(h: &Harness, path: &str) -> codediff::controller::DiffView {
    let m = h.mark();
    h.ctl.send(Command::Select(Selection::Change {
        side: ChangeSide::Unstaged,
        path: path.into(),
    }));
    match h.wait_for(m, |e| matches!(e, Event::Diff(Some(_)))).1 {
        Event::Diff(Some(v)) => v,
        _ => unreachable!(),
    }
}

/// (header, fingerprint) of each hunk in the view, as the UI would send them.
fn hunk_keys(v: &codediff::controller::DiffView) -> Vec<(String, u64)> {
    match &v.file.body {
        DiffBody::Text(h) => h
            .iter()
            .map(|h| (h.header.clone(), hunk_fingerprint(h)))
            .collect(),
        other => panic!("not text: {other:?}"),
    }
}

#[test]
fn open_emits_repo_status_history() {
    let t = TestRepo::new();
    t.write("a.txt", b"a\n");
    t.stage("a.txt");
    let c1 = t.commit("first");
    t.write("b.txt", b"b\n");
    let h = Harness::new();
    let m = h.mark();
    h.ctl.send(Command::OpenRepo(t.path().to_path_buf()));
    let (i_open, e) = h.wait_for(m, |e| matches!(e, Event::RepoOpened { .. }));
    let Event::RepoOpened { name, recent, .. } = e else {
        unreachable!()
    };
    assert_eq!(name, t.path().file_name().unwrap().to_string_lossy());
    assert_eq!(recent.len(), 1);
    let (i_status, _) = h.wait_for(m, |e| status_has(e, &[], &["b.txt"]));
    let (i_hist, e) = h.wait_for(m, |e| matches!(e, Event::History { .. }));
    let Event::History {
        commits,
        append,
        new_ids,
    } = e
    else {
        unreachable!()
    };
    assert_eq!(commits.iter().map(|c| c.id).collect::<Vec<_>>(), vec![c1]);
    assert!(!append);
    assert!(new_ids.is_empty());
    let (i_diff, _) = h.wait_for(m, |e| matches!(e, Event::Diff(None)));
    assert!(i_open < i_status && i_status < i_hist && i_hist < i_diff);
    assert!(h.recent_file().exists());
}

#[test]
fn open_non_repo_emits_error() {
    let dir = tempfile::tempdir().unwrap();
    let h = Harness::new();
    h.ctl.send(Command::OpenRepo(dir.path().to_path_buf()));
    h.wait_for(
        0,
        |e| matches!(e, Event::Error(m) if m.contains("Not a Git repository")),
    );
    assert!(!h
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, Event::RepoOpened { .. })));
}

#[test]
fn open_error_keeps_previous_repo() {
    let t = two_hunk_repo();
    let bad = tempfile::tempdir().unwrap();
    let h = Harness::new();
    h.open(t.path());
    let m = h.mark();
    h.ctl.send(Command::OpenRepo(bad.path().to_path_buf()));
    h.wait_for(m, |e| matches!(e, Event::Error(_)));
    let m = h.mark();
    h.ctl.send(Command::Refresh);
    h.wait_for(m, |e| status_has(e, &[], &["f.txt"]));
}

#[test]
fn stage_file_emits_updated_status() {
    let t = two_hunk_repo();
    let h = Harness::new();
    let m = h.open(t.path());
    h.wait_for(0, |e| status_has(e, &[], &["f.txt"]));
    h.ctl.send(Command::StageFile("f.txt".into()));
    h.wait_for(m, |e| status_has(e, &["f.txt"], &[]));
    assert!(t.index_blob("f.txt").unwrap().starts_with(b"1\ntwo\n"));
}

#[test]
fn stale_hunk_emits_error_then_refresh() {
    let t = two_hunk_repo();
    let h = Harness::new();
    h.open(t.path());
    let v = select_unstaged(&h, "f.txt");
    let (real, fp) = hunk_keys(&v)[0].clone();

    // Header differs from what the view showed.
    let m = h.mark();
    h.ctl.send(Command::StageHunk {
        path: "f.txt".into(),
        hunk_index: 0,
        header: "@@ bogus @@".into(),
        fingerprint: fp,
    });
    let (i_err, e) = h.wait_for(m, |e| matches!(e, Event::Error(_)));
    assert!(matches!(e, Event::Error(ref s) if s == "File changed, refreshed. Try again."));
    h.wait_for(i_err, |e| matches!(e, Event::Status(_)));

    // Out of range index.
    let m = h.mark();
    h.ctl.send(Command::DiscardHunk {
        path: "f.txt".into(),
        hunk_index: 9,
        header: real.clone(),
        fingerprint: fp,
    });
    let (i_err, _) = h.wait_for(m, |e| matches!(e, Event::Error(_)));
    h.wait_for(i_err, |e| matches!(e, Event::Status(_)));

    // Wrong side: unstage from an unstaged view.
    let m = h.mark();
    h.ctl.send(Command::UnstageHunk {
        path: "f.txt".into(),
        hunk_index: 0,
        header: real,
        fingerprint: fp,
    });
    let (i_err, _) = h.wait_for(m, |e| matches!(e, Event::Error(_)));
    h.wait_for(i_err, |e| matches!(e, Event::Status(_)));

    assert_eq!(
        t.index_blob("f.txt").unwrap(),
        numbered(),
        "index untouched"
    );
}

#[test]
fn stage_hunk_uses_current_view() {
    let t = two_hunk_repo();
    let h = Harness::new();
    h.open(t.path());
    let v = select_unstaged(&h, "f.txt");
    let (header, fingerprint) = hunk_keys(&v)[0].clone();
    let m = h.mark();
    h.ctl.send(Command::StageHunk {
        path: "f.txt".into(),
        hunk_index: 0,
        header,
        fingerprint,
    });
    h.wait_for(m, |e| status_has(e, &["f.txt"], &["f.txt"]));
    let idx = String::from_utf8(t.index_blob("f.txt").unwrap()).unwrap();
    assert!(idx.contains("two\n") && !idx.contains("eighteen"));
    assert!(!h.events.lock().unwrap()[m..]
        .iter()
        .any(|e| matches!(e, Event::Error(_))));
}

/// Sends `cmd`, expects the Stale error followed by a refresh Status.
fn expect_stale(h: &Harness, cmd: Command) {
    let m = h.mark();
    h.ctl.send(cmd);
    let (i_err, e) = h.wait_for(m, |e| matches!(e, Event::Error(_)));
    assert!(matches!(e, Event::Error(ref s) if s == "File changed, refreshed. Try again."));
    h.wait_for(i_err, |e| matches!(e, Event::Status(_)));
}

/// Waits for a refreshed unstaged Diff whose hunk 0 contains `needle`.
fn wait_view_with(h: &Harness, from: usize, needle: &str) {
    h.wait_for(from, |e| match e {
        Event::Diff(Some(v)) => matches!(&v.file.body, DiffBody::Text(hs)
            if hs[0].lines.iter().any(|l| l.text.contains(needle))),
        _ => false,
    });
}

#[test]
fn stale_after_line_count_change_is_rejected() {
    let t = two_hunk_repo();
    let h = Harness::new();
    h.open(t.path());
    let v = select_unstaged(&h, "f.txt");
    let (old_header, old_fp) = hunk_keys(&v)[0].clone();

    let m = h.mark();
    let edited = String::from_utf8(t.read("f.txt"))
        .unwrap()
        .replacen("two\n", "two\nextra\n", 1);
    t.write("f.txt", edited.as_bytes());
    wait_view_with(&h, m, "extra");

    expect_stale(
        &h,
        Command::DiscardHunk {
            path: "f.txt".into(),
            hunk_index: 0,
            header: old_header,
            fingerprint: old_fp,
        },
    );
    assert_eq!(t.read("f.txt"), edited.as_bytes(), "workdir untouched");
    assert_eq!(
        t.index_blob("f.txt").unwrap(),
        numbered(),
        "index untouched"
    );
}

#[test]
fn stale_after_same_header_rewrite_is_rejected() {
    let t = two_hunk_repo();
    let h = Harness::new();
    h.open(t.path());
    let v = select_unstaged(&h, "f.txt");
    let (old_header, old_fp) = hunk_keys(&v)[0].clone();

    let m = h.mark();
    let edited = String::from_utf8(t.read("f.txt"))
        .unwrap()
        .replacen("two\n", "TWO\n", 1);
    t.write("f.txt", edited.as_bytes());
    wait_view_with(&h, m, "TWO");
    let fresh = h.events.lock().unwrap().iter().rev().find_map(|e| match e {
        Event::Diff(Some(v)) => Some(v.clone()),
        _ => None,
    });
    let fresh = fresh.expect("refreshed view");
    assert_eq!(
        hunk_keys(&fresh)[0].0,
        old_header,
        "same header, new content"
    );

    for cmd in [
        Command::StageHunk {
            path: "f.txt".into(),
            hunk_index: 0,
            header: old_header.clone(),
            fingerprint: old_fp,
        },
        Command::DiscardHunk {
            path: "f.txt".into(),
            hunk_index: 0,
            header: old_header.clone(),
            fingerprint: old_fp,
        },
    ] {
        expect_stale(&h, cmd);
    }
    assert_eq!(t.read("f.txt"), edited.as_bytes(), "workdir untouched");
    assert_eq!(
        t.index_blob("f.txt").unwrap(),
        numbered(),
        "index untouched"
    );
}

#[test]
fn external_write_triggers_status_refresh() {
    let t = TestRepo::new();
    t.write("a.txt", b"a\n");
    t.stage("a.txt");
    t.commit("first");
    let h = Harness::new();
    let m = h.open(t.path());
    t.write("new.txt", b"hello\n");
    h.wait_for(m, |e| status_has(e, &[], &["new.txt"]));
}

#[test]
fn new_commit_marks_new_ids_and_extends_range() {
    let t = TestRepo::new();
    t.write("a.txt", b"one\n");
    t.stage("a.txt");
    let c1 = t.commit("c1");
    t.write("a.txt", b"two\n");
    t.stage("a.txt");
    let c2 = t.commit("c2");
    let h = Harness::new();
    h.open(t.path());

    let m = h.mark();
    h.ctl.send(Command::Select(Selection::Range {
        base: c1,
        head: c2,
        path: None,
        follows_head: true,
    }));
    let (_, e) = h.wait_for(m, |e| matches!(e, Event::Range(Some(_))));
    let Event::Range(Some(r)) = e else {
        unreachable!()
    };
    assert_eq!(
        (r.head, r.commit_count, r.additions, r.deletions),
        (c2, 1, 1, 1)
    );
    h.wait_for(
        m,
        |e| matches!(e, Event::Files(f) if f.len() == 1 && f[0].path == "a.txt"),
    );
    h.wait_for(
        m,
        |e| matches!(e, Event::Diff(Some(v)) if v.file.path == "a.txt"),
    );

    let m = h.mark();
    t.write("b.txt", b"b\n");
    t.stage("b.txt");
    let c3 = t.commit("c3");
    let (_, e) = h.wait_for(
        m,
        |e| matches!(e, Event::History { new_ids, .. } if !new_ids.is_empty()),
    );
    let Event::History {
        new_ids,
        commits,
        append,
    } = e
    else {
        unreachable!()
    };
    assert_eq!(new_ids, vec![c3]);
    assert_eq!(commits.len(), 3);
    assert!(!append);
    let (_, e) = h.wait_for(m, |e| matches!(e, Event::Range(Some(_))));
    let Event::Range(Some(r)) = e else {
        unreachable!()
    };
    assert_eq!((r.base, r.head, r.commit_count), (c1, c3, 2));
    h.wait_for(m, |e| matches!(e, Event::Files(f) if f.len() == 2));
}

#[test]
fn select_commit_emits_files_and_first_diff() {
    let t = TestRepo::new();
    t.write("b.txt", b"b\n");
    t.write("a.txt", b"a\n");
    t.stage("a.txt");
    t.stage("b.txt");
    let c1 = t.commit("c1");
    let h = Harness::new();
    h.open(t.path());
    let m = h.mark();
    h.ctl
        .send(Command::Select(Selection::Commit { id: c1, path: None }));
    let (i_files, _) = h.wait_for(m, |e| matches!(e, Event::Files(f) if f.len() == 2));
    let (i_diff, e) = h.wait_for(m, |e| matches!(e, Event::Diff(Some(_))));
    let Event::Diff(Some(v)) = e else {
        unreachable!()
    };
    assert_eq!(v.file.path, "a.txt");
    assert_eq!(v.side, None);
    assert!(!v.rows.is_empty());
    assert!(i_files < i_diff);
}

#[test]
fn set_dark_reemits_diff() {
    let t = two_hunk_repo();
    let h = Harness::new();
    h.open(t.path());
    let light = select_unstaged(&h, "f.txt");
    let m = h.mark();
    h.ctl.send(Command::SetDark(true));
    let (_, e) = h.wait_for(m, |e| matches!(e, Event::Diff(Some(_))));
    let Event::Diff(Some(dark)) = e else {
        unreachable!()
    };
    assert_eq!(dark.file, light.file);
    assert_ne!(dark.rows, light.rows);
}

#[test]
fn load_full_diff_reemits_with_force_full() {
    let t = TestRepo::new();
    t.write("big.txt", b"x\n");
    t.stage("big.txt");
    t.commit("c1");
    let body: String = (0..6_000).map(|i| format!("line {i}\n")).collect();
    t.write("big.txt", body.as_bytes());
    let h = Harness::new();
    h.open(t.path());
    let v = select_unstaged(&h, "big.txt");
    assert!(
        matches!(v.file.body, DiffBody::TooLarge { .. }),
        "{:?}",
        v.file.body
    );
    let m = h.mark();
    h.ctl.send(Command::LoadFullDiff);
    let (_, e) = h.wait_for(m, |e| matches!(e, Event::Diff(Some(_))));
    let Event::Diff(Some(full)) = e else {
        unreachable!()
    };
    assert!(matches!(full.file.body, DiffBody::Text(_)));
}

#[test]
fn load_more_history_appends() {
    let t = TestRepo::new();
    let mut ids: Vec<Oid> = Vec::new();
    for i in 0..205 {
        t.write("a.txt", format!("{i}\n").as_bytes());
        t.stage("a.txt");
        ids.push(t.commit(&format!("c{i}")));
    }
    let h = Harness::new();
    h.open(t.path());
    let first = h.events.lock().unwrap().iter().find_map(|e| match e {
        Event::History { commits, .. } => Some(commits.len()),
        _ => None,
    });
    assert_eq!(first, Some(200));
    let m = h.mark();
    h.ctl.send(Command::LoadMoreHistory);
    let (_, e) = h.wait_for(m, |e| matches!(e, Event::History { append: true, .. }));
    let Event::History { commits, .. } = e else {
        unreachable!()
    };
    assert_eq!(
        commits.iter().map(|c| c.id).collect::<Vec<_>>(),
        ids[..5].iter().rev().cloned().collect::<Vec<_>>()
    );
}

#[test]
fn no_refresh_loop_when_idle() {
    let t = two_hunk_repo();
    let h = Harness::new();
    h.open(t.path());
    select_unstaged(&h, "f.txt");
    std::thread::sleep(Duration::from_millis(500));
    let m = h.mark();
    std::thread::sleep(Duration::from_secs(1));
    let n = h.events.lock().unwrap()[m..]
        .iter()
        .filter(|e| matches!(e, Event::Status(_)))
        .count();
    assert!(n <= 1, "status events while idle: {n}");
}

#[test]
fn drop_and_shutdown_end_worker() {
    let h = Harness::new();
    h.ctl.send(Command::Shutdown);
    std::thread::sleep(Duration::from_millis(50));
    h.ctl.send(Command::Refresh); // worker gone: must not panic or block
    drop(h); // join must not deadlock
}

#[test]
fn stale_file_discard_is_rejected() {
    let t = two_hunk_repo();
    let h = Harness::new();
    h.open(t.path());
    // The dialog opens on this view and captures its fingerprint.
    let v = select_unstaged(&h, "f.txt");
    let fp = file_fingerprint(&v.file);

    // An agent rewrites the file before the user confirms. No wait for the
    // watcher: the check must not depend on a refresh having happened.
    let edited = String::from_utf8(t.read("f.txt"))
        .unwrap()
        .replacen("10\n", "ten\n", 1);
    t.write("f.txt", edited.as_bytes());
    expect_stale(
        &h,
        Command::DiscardFile {
            path: "f.txt".into(),
            fingerprint: fp,
        },
    );
    assert_eq!(t.read("f.txt"), edited.as_bytes(), "workdir untouched");
}

#[test]
fn fresh_file_discard_discards() {
    let t = two_hunk_repo();
    let h = Harness::new();
    h.open(t.path());
    let v = select_unstaged(&h, "f.txt");
    let m = h.mark();
    h.ctl.send(Command::DiscardFile {
        path: "f.txt".into(),
        fingerprint: file_fingerprint(&v.file),
    });
    h.wait_for(m, |e| status_has(e, &[], &[]));
    assert_eq!(t.read("f.txt"), numbered());
    assert!(!h.events.lock().unwrap()[m..]
        .iter()
        .any(|e| matches!(e, Event::Error(_))));
}

fn select_range(h: &Harness, base: Oid, head: Oid, follows_head: bool) -> usize {
    let m = h.mark();
    h.ctl.send(Command::Select(Selection::Range {
        base,
        head,
        path: None,
        follows_head,
    }));
    m
}

fn range_at(h: &Harness, from: usize) -> codediff::controller::RangeInfo {
    match h.wait_for(from, |e| matches!(e, Event::Range(Some(_)))).1 {
        Event::Range(Some(r)) => r,
        _ => unreachable!(),
    }
}

fn commit_file(t: &TestRepo, name: &str) -> Oid {
    t.write(name, name.as_bytes());
    t.stage(name);
    t.commit(name)
}

#[test]
fn follows_head_range_catches_up_after_tab_switch() {
    let t = TestRepo::new();
    let c1 = commit_file(&t, "a.txt");
    let c2 = commit_file(&t, "b.txt");
    let h = Harness::new();
    h.open(t.path());
    let m = select_range(&h, c1, c2, true);
    assert_eq!(range_at(&h, m).head, c2);

    // Switch to the Changes tab: the controller selection becomes a Change.
    let m = h.mark();
    h.ctl.send(Command::Select(Selection::Change {
        side: ChangeSide::Unstaged,
        path: "none.txt".into(),
    }));
    h.wait_for(m, |e| matches!(e, Event::Diff(None)));

    // An agent commits while the range is not selected.
    let m = h.mark();
    let c3 = commit_file(&t, "c.txt");
    h.wait_for(m, |e| matches!(e, Event::History { new_ids, .. } if new_ids == &vec![c3]));

    // Back to History: the UI re-sends its stale head.
    let m = select_range(&h, c1, c2, true);
    let r = range_at(&h, m);
    assert_eq!((r.base, r.head, r.commit_count), (c1, c3, 2));
    h.wait_for(m, |e| matches!(e, Event::Files(f) if f.len() == 2));

    // And it keeps following on later refreshes.
    let m = h.mark();
    let c4 = commit_file(&t, "d.txt");
    let r = range_at(&h, m);
    assert_eq!((r.head, r.commit_count), (c4, 3));
}

#[test]
fn fixed_range_does_not_follow_head() {
    let t = TestRepo::new();
    let c1 = commit_file(&t, "a.txt");
    let c2 = commit_file(&t, "b.txt");
    let h = Harness::new();
    h.open(t.path());
    let m = select_range(&h, c1, c2, false);
    assert_eq!(range_at(&h, m).head, c2);
    let m = h.mark();
    let c3 = commit_file(&t, "c.txt");
    let (i, _) = h.wait_for(m, |e| matches!(e, Event::History { new_ids, .. } if new_ids == &vec![c3]));
    // The refresh that reported c3 finishes with a Diff; no Range change before it.
    let (j, _) = h.wait_for(i, |e| matches!(e, Event::Diff(_)));
    assert!(!h.events.lock().unwrap()[m..=j]
        .iter()
        .any(|e| matches!(e, Event::Range(_))));
}

#[test]
fn linked_worktree_index_and_commit_trigger_refresh() {
    let t = TestRepo::new();
    t.write("a.txt", b"a\n");
    t.stage("a.txt");
    t.commit("first");
    let parent = tempfile::tempdir().unwrap();
    let wt_path = parent.path().join("wt");
    t.repo.worktree("wt", &wt_path, None).unwrap();
    let wt = git2::Repository::open(&wt_path).unwrap();
    std::fs::write(wt_path.join("new.txt"), b"hello\n").unwrap();
    std::thread::sleep(Duration::from_millis(300));

    let h = Harness::new();
    let m = h.open(&wt_path);
    h.wait_for(0, |e| status_has(e, &[], &["new.txt"]));

    // Only Git state changes from here on, no workdir writes.
    let mut idx = wt.index().unwrap();
    idx.add_path(std::path::Path::new("new.txt")).unwrap();
    idx.write().unwrap();
    let m = h.wait_for(m, |e| status_has(e, &["new.txt"], &[])).0;

    let tree = wt.find_tree(idx.write_tree().unwrap()).unwrap();
    let sig = wt.signature().unwrap();
    let parent_commit = wt.head().unwrap().peel_to_commit().unwrap();
    let c2 = wt
        .commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&parent_commit])
        .unwrap();
    h.wait_for(m, |e| matches!(e, Event::History { new_ids, .. } if new_ids == &vec![c2]));
}
