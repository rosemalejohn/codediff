use codediff::model::{DiffLine, Hunk, LineKind};
use codediff::sbs::{build_rows, build_rows_expanded, ExpandDir, Expansion, GapCtl, Row};

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

fn at(start_old: u32, start_new: u32, lines: Vec<DiffLine>) -> Hunk {
    let old_lines = lines.iter().filter(|l| l.kind != LineKind::Added).count() as u32;
    let new_lines = lines.iter().filter(|l| l.kind != LineKind::Removed).count() as u32;
    Hunk { header: format!("@@ -{start_old} +{start_new} @@"), old_start: start_old, old_lines, new_start: start_new, new_lines, lines }
}

/// 100-line old file; hunk 0 replaces line 40 with two lines, hunk 1 removes line 46.
fn file() -> (Vec<String>, Vec<Hunk>) {
    let old: Vec<String> = (1..=100).map(|i| format!("l{i}")).collect();
    let h0 = at(40, 40, vec![
        dl(LineKind::Removed, Some(40), None, "l40"),
        dl(LineKind::Added, None, Some(40), "a"),
        dl(LineKind::Added, None, Some(41), "b"),
    ]);
    let h1 = at(46, 46, vec![dl(LineKind::Removed, Some(46), None, "l46")]);
    (old, vec![h0, h1])
}

/// (old_no, new_no) of every context line.
fn context(rows: &[Row]) -> Vec<(u32, u32)> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Line { left: Some(l), right: Some(r) } if l.kind == LineKind::Context => Some((l.no, r.no)),
            _ => None,
        })
        .collect()
}

fn header_gap(rows: &[Row], hunk: usize) -> Option<GapCtl> {
    rows.iter().find_map(|r| match r {
        Row::HunkHeader { hunk_index, gap, .. } if *hunk_index == hunk => Some(*gap),
        _ => None,
    })?
}

#[test]
fn collapsed_gaps_show_controls_only() {
    let (old, hunks) = file();
    let rows = build_rows_expanded(&hunks, &old, &[]);
    assert!(context(&rows).is_empty());
    assert_eq!(header_gap(&rows, 0), Some(GapCtl { gap: 0, hidden: 39, up: true, down: false, all: false }));
    // Lines 41..=45: small middle gap, one expand-all control.
    assert_eq!(header_gap(&rows, 1), Some(GapCtl { gap: 1, hidden: 5, up: false, down: false, all: true }));
    assert!(matches!(rows.last(), Some(Row::Gap(GapCtl { gap: 2, hidden: 54, down: true, up: false, .. }))));
}

#[test]
fn expand_up_reveals_lines_above_first_hunk() {
    let (old, hunks) = file();
    let mut e = Expansion::default();
    e.apply(ExpandDir::Up);
    let rows = build_rows_expanded(&hunks, &old, &[e]);
    let ctx = context(&rows);
    assert_eq!(ctx.first(), Some(&(20, 20)));
    assert_eq!(ctx.len(), 20);
    assert_eq!(header_gap(&rows, 0).unwrap().hidden, 19);
    // Revealed lines sit between the header and the hunk.
    assert!(matches!(rows[0], Row::HunkHeader { hunk_index: 0, .. }));
}

#[test]
fn expand_all_middle_and_down_trailing_shift_new_numbers() {
    let (old, hunks) = file();
    let mut mid = Expansion::default();
    mid.apply(ExpandDir::All);
    let mut tail = Expansion::default();
    tail.apply(ExpandDir::Down);
    let rows = build_rows_expanded(&hunks, &old, &[Expansion::default(), mid, tail]);
    let ctx = context(&rows);
    // Middle gap: old 41..=45 are new 42..=46 (one extra line added above).
    assert_eq!(&ctx[..5], &[(41, 42), (42, 43), (43, 44), (44, 45), (45, 46)]);
    assert_eq!(header_gap(&rows, 1), None);
    // Trailing: old 47.. are new 47.. (one added, one removed above).
    assert_eq!(ctx[5], (47, 47));
    assert_eq!(ctx.len(), 25);
    assert!(matches!(rows.last(), Some(Row::Gap(GapCtl { hidden: 34, .. }))));
}

#[test]
fn no_old_lines_means_no_controls() {
    let (_, hunks) = file();
    let rows = build_rows_expanded(&hunks, &[], &[]);
    assert_eq!(header_gap(&rows, 0), None);
    assert!(!rows.iter().any(|r| matches!(r, Row::Gap(_))));
}
