use codediff::model::{DiffLine, Hunk, LineKind};
use codediff::sbs::{build_rows, Row};

fn dl(kind: LineKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
    DiffLine { kind, old_no: old, new_no: new, text: text.to_string() }
}

fn hunk(lines: Vec<DiffLine>) -> Hunk {
    Hunk { header: "@@ -1 +1 @@".into(), old_start: 1, old_lines: 1, new_start: 1, new_lines: 1, lines }
}

#[test]
fn context_line_on_both_sides() {
    let rows = build_rows(&[hunk(vec![dl(LineKind::Context, Some(1), Some(1), "x")])]);
    match &rows[1] {
        Row::Line { left: Some(l), right: Some(r) } => {
            assert_eq!(l.no, 1);
            assert_eq!(r.no, 1);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn removed_then_added_paired() {
    let rows = build_rows(&[hunk(vec![
        dl(LineKind::Removed, Some(1), None, "a"),
        dl(LineKind::Added, None, Some(1), "b"),
        dl(LineKind::Added, None, Some(2), "c"),
    ])]);
    assert_eq!(rows.len(), 3);
    assert!(matches!(&rows[1], Row::Line { left: Some(_), right: Some(_) }));
    assert!(matches!(&rows[2], Row::Line { left: None, right: Some(_) }));
}

#[test]
fn hunk_header_row_first() {
    let h = || hunk(vec![dl(LineKind::Context, Some(1), Some(1), "x")]);
    let rows = build_rows(&[h(), h()]);
    assert!(matches!(&rows[0], Row::HunkHeader { hunk_index: 0, .. }));
    assert!(matches!(&rows[2], Row::HunkHeader { hunk_index: 1, .. }));
}

#[test]
fn word_level_marks_changed_words() {
    let old = "let db = connect();";
    let rows = build_rows(&[hunk(vec![
        dl(LineKind::Removed, Some(1), None, old),
        dl(LineKind::Added, None, Some(1), "let db = connect(&cfg)?;"),
    ])]);
    let Row::Line { left: Some(l), .. } = &rows[1] else { panic!() };
    let joined: String = l.segs.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(joined, old);
    assert!(l.segs.iter().any(|s| s.changed && (s.text.contains("connect") || s.text.contains("()"))));
    assert_eq!(l.segs[0].text, "let");
    assert!(!l.segs[0].changed);
}

#[test]
fn only_additions_has_none_left() {
    let rows = build_rows(&[hunk(vec![dl(LineKind::Added, None, Some(1), "new")])]);
    match &rows[1] {
        Row::Line { left: None, right: Some(r) } => {
            assert_eq!(r.segs.len(), 1);
            assert!(!r.segs[0].changed);
        }
        other => panic!("unexpected {other:?}"),
    }
}
