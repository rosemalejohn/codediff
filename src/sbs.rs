//! Side-by-side row building with word-level highlighting.
use crate::model::{Hunk, LineKind};
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, PartialEq)]
pub struct Seg {
    pub text: String,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub no: u32,
    pub kind: LineKind,
    pub segs: Vec<Seg>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    HunkHeader { hunk_index: usize, header: String },
    Line { left: Option<Cell>, right: Option<Cell> },
}

fn plain(no: u32, kind: LineKind, text: &str) -> Cell {
    Cell { no, kind, segs: vec![Seg { text: text.to_string(), changed: false }] }
}

fn push_seg(segs: &mut Vec<Seg>, text: &str, changed: bool) {
    if let Some(last) = segs.last_mut() {
        if changed && last.changed {
            last.text.push_str(text);
            return;
        }
    }
    segs.push(Seg { text: text.to_string(), changed });
}

fn word_segs(old: &str, new: &str) -> (Vec<Seg>, Vec<Seg>) {
    let few = old.split_whitespace().count() < 3 || new.split_whitespace().count() < 3;
    let diff = if few { TextDiff::from_chars(old, new) } else { TextDiff::from_words(old, new) };
    let (mut l, mut r) = (Vec::new(), Vec::new());
    for c in diff.iter_all_changes() {
        match c.tag() {
            ChangeTag::Equal => {
                push_seg(&mut l, c.value(), false);
                push_seg(&mut r, c.value(), false);
            }
            ChangeTag::Delete => push_seg(&mut l, c.value(), true),
            ChangeTag::Insert => push_seg(&mut r, c.value(), true),
        }
    }
    (l, r)
}

fn flush(rows: &mut Vec<Row>, removed: &mut Vec<(u32, String)>, added: &mut Vec<(u32, String)>) {
    for i in 0..removed.len().max(added.len()) {
        let (left, right) = match (removed.get(i), added.get(i)) {
            (Some((ln, lt)), Some((rn, rt))) => {
                let (ls, rs) = word_segs(lt, rt);
                (
                    Some(Cell { no: *ln, kind: LineKind::Removed, segs: ls }),
                    Some(Cell { no: *rn, kind: LineKind::Added, segs: rs }),
                )
            }
            (Some((ln, lt)), None) => (Some(plain(*ln, LineKind::Removed, lt)), None),
            (None, Some((rn, rt))) => (None, Some(plain(*rn, LineKind::Added, rt))),
            (None, None) => unreachable!(),
        };
        rows.push(Row::Line { left, right });
    }
    removed.clear();
    added.clear();
}

pub fn build_rows(hunks: &[Hunk]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (hunk_index, h) in hunks.iter().enumerate() {
        rows.push(Row::HunkHeader { hunk_index, header: h.header.clone() });
        let (mut removed, mut added) = (Vec::new(), Vec::new());
        for line in &h.lines {
            match line.kind {
                LineKind::Removed => {
                    // A removed line after added lines starts a new run.
                    if !added.is_empty() {
                        flush(&mut rows, &mut removed, &mut added);
                    }
                    removed.push((line.old_no.unwrap_or(0), line.text.clone()));
                }
                LineKind::Added => added.push((line.new_no.unwrap_or(0), line.text.clone())),
                LineKind::Context => {
                    flush(&mut rows, &mut removed, &mut added);
                    rows.push(Row::Line {
                        left: Some(plain(line.old_no.unwrap_or(0), LineKind::Context, &line.text)),
                        right: Some(plain(line.new_no.unwrap_or(0), LineKind::Context, &line.text)),
                    });
                }
            }
        }
        flush(&mut rows, &mut removed, &mut added);
    }
    rows
}
