mod common;
use codediff::history::{list_commits, relative_time};
use common::TestRepo;

fn make(n: usize) -> TestRepo {
    let t = TestRepo::new();
    for i in 1..=n {
        t.write("f.txt", format!("{i}\n").as_bytes());
        t.stage("f.txt");
        t.commit(&format!("c{i}"));
    }
    t
}

#[test]
fn lists_newest_first() {
    let t = make(3);
    let v = list_commits(&t.repo, 0, 10).unwrap();
    let s: Vec<_> = v.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(s, vec!["c3", "c2", "c1"]);
    assert_eq!(v[0].short_id.len(), 7);
    assert_eq!(v[0].author, "Test");
}

#[test]
fn paging_offset_limit() {
    let t = make(5);
    let v = list_commits(&t.repo, 2, 2).unwrap();
    let s: Vec<_> = v.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(s, vec!["c3", "c2"]);
}

#[test]
fn history_on_unborn_repo_is_empty() {
    let t = TestRepo::new();
    assert!(list_commits(&t.repo, 0, 10).unwrap().is_empty());
}

#[test]
fn summary_is_first_line_only() {
    let t = TestRepo::new();
    t.write("a", b"1");
    t.stage("a");
    t.commit("first line\n\nbody text");
    let v = list_commits(&t.repo, 0, 10).unwrap();
    assert_eq!(v[0].summary, "first line");
}

#[test]
fn relative_time_buckets() {
    let now = 1_700_000_000;
    assert_eq!(relative_time(now, now), "just now");
    assert_eq!(relative_time(now, now - 59), "just now");
    assert_eq!(relative_time(now, now - 300), "5m ago");
    assert_eq!(relative_time(now, now - 10800), "3h ago");
    assert_eq!(relative_time(now, now - 172800), "2d ago");
    // 1_700_000_000 - 40d = 1_696_544_000 => 2023-10-05
    assert_eq!(relative_time(now, now - 40 * 86400), "2023-10-05");
}
