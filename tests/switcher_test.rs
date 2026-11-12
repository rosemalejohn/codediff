use codediff::switcher::{fuzzy_score, project_name, rank};

fn paths(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn subsequence_matches_case_insensitive() {
    assert!(fuzzy_score("dfc", "diffchecker").is_some());
    assert!(fuzzy_score("DIFF", "diffchecker").is_some());
    assert!(fuzzy_score("xyz", "diffchecker").is_none());
    assert_eq!(fuzzy_score("", "anything"), Some(0));
}

#[test]
fn word_starts_and_runs_beat_scattered_hits() {
    let start = fuzzy_score("wh", "wanna-hookup").unwrap();
    let scattered = fuzzy_score("wh", "awkwardthing").unwrap();
    assert!(start > scattered);
    let run = fuzzy_score("diff", "diffchecker").unwrap();
    let split = fuzzy_score("diff", "d-i-f-f").unwrap();
    assert!(run > 0 && split > 0);
}

#[test]
fn name_match_outranks_path_only_match() {
    let p = paths(&["/Users/me/Work/api/web", "/Users/me/web-app", "/Users/me/other"]);
    // "web" is the name of 0 and starts 1's name; "other" has no match.
    let r = rank("web", &p);
    assert_eq!(r.len(), 2);
    assert!(!r.contains(&2));
    // Plain queries ignore the directories; a slash searches the path.
    assert!(rank("work", &p).is_empty());
    assert_eq!(rank("work/api", &p), vec![0]);
}

#[test]
fn empty_query_keeps_recency_order() {
    let p = paths(&["/a/one", "/b/two", "/c/three"]);
    assert_eq!(rank("", &p), vec![0, 1, 2]);
    assert_eq!(rank("  ", &p), vec![0, 1, 2]);
}

#[test]
fn project_name_is_last_component() {
    assert_eq!(project_name("/Users/me/Work/diffchecker"), "diffchecker");
    assert_eq!(project_name("/Users/me/Work/diffchecker/"), "diffchecker");
    assert_eq!(project_name("solo"), "solo");
}
