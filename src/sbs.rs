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

/// Lines revealed by one click of an expand control.
pub const EXPAND_STEP: u32 = 20;

/// How far one gap of unchanged lines is expanded. Gap `i < hunks.len()` is
/// the gap before hunk `i`; gap `hunks.len()` is the one after the last hunk.
/// `down` counts lines revealed below the previous hunk, `up` lines revealed
/// above the next one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Expansion {
    pub down: u32,
    pub up: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpandDir {
    /// Reveal lines just above the next hunk.
    Up,
    /// Reveal lines just below the previous hunk.
    Down,
    /// Reveal the whole gap.
    All,
}

impl Expansion {
    pub fn apply(&mut self, dir: ExpandDir) {
        match dir {
            ExpandDir::Up => self.up = self.up.saturating_add(EXPAND_STEP),
            ExpandDir::Down => self.down = self.down.saturating_add(EXPAND_STEP),
            ExpandDir::All => self.down = u32::MAX,
        }
    }
}

/// Expand controls of a gap that still hides lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapCtl {
    pub gap: usize,
    pub hidden: u32,
    pub up: bool,
    pub down: bool,
    pub all: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// `gap`: controls of the gap before this hunk, None when nothing is hidden.
    HunkHeader { hunk_index: usize, header: String, gap: Option<GapCtl> },
    Line { left: Option<Cell>, right: Option<Cell> },
    /// Hidden lines after the last hunk.
    Gap(GapCtl),
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

/// First line a hunk side covers. A side with no lines starts after `start`.
fn side_begin(start: u32, len: u32) -> u32 {
    if len == 0 {
        start + 1
    } else {
        start
    }
}

/// One gap of unchanged lines: old-side span `[from, to)` (1-based) and the
/// new-minus-old line number offset inside it.
struct Span {
    from: u32,
    to: u32,
    delta: i64,
}

fn spans(hunks: &[Hunk], total_old: u32) -> Vec<Span> {
    let limit = total_old + 1;
    let mut out = Vec::with_capacity(hunks.len() + 1);
    let mut from = 1;
    for h in hunks {
        let ob = side_begin(h.old_start, h.old_lines);
        let nb = side_begin(h.new_start, h.new_lines);
        let to = ob.min(limit).max(from);
        out.push(Span { from, to, delta: nb as i64 - ob as i64 });
        from = (ob + h.old_lines).max(to);
    }
    if let Some(h) = hunks.last() {
        let oe = side_begin(h.old_start, h.old_lines) + h.old_lines;
        let ne = side_begin(h.new_start, h.new_lines) + h.new_lines;
        out.push(Span { from, to: limit.max(from), delta: ne as i64 - oe as i64 });
    }
    out
}

fn push_context(rows: &mut Vec<Row>, old_lines: &[String], span: &Span, from: u32, to: u32) {
    for no in from..to {
        let Some(text) = old_lines.get(no as usize - 1) else { break };
        let new_no = (no as i64 + span.delta).max(0) as u32;
        rows.push(Row::Line {
            left: Some(plain(no, LineKind::Context, text)),
            right: Some(plain(new_no, LineKind::Context, text)),
        });
    }
}

pub fn build_rows(hunks: &[Hunk]) -> Vec<Row> {
    build_rows_expanded(hunks, &[], &[])
}

/// Lines a gap reveals below the previous hunk and above the next one, plus
/// its controls when lines stay hidden.
fn cut(i: usize, last: usize, size: u32, e: Expansion) -> (u32, u32, Option<GapCtl>) {
    let asked = e.down.saturating_add(e.up);
    let (down, up) = if i == last {
        (asked.min(size), 0)
    } else if i == 0 {
        (0, asked.min(size))
    } else if asked >= size {
        (size, 0)
    } else {
        (e.down, e.up)
    };
    let hidden = size - down - up;
    let ctl = (hidden > 0).then(|| {
        let middle = i != 0 && i != last;
        GapCtl {
            gap: i,
            hidden,
            up: i == 0 || (middle && hidden > EXPAND_STEP),
            down: i == last || (middle && hidden > EXPAND_STEP),
            all: middle && hidden <= EXPAND_STEP,
        }
    });
    (down, up, ctl)
}

/// Rows with the unchanged lines between hunks revealed as far as `expand`
/// says (missing entries mean collapsed). `old_lines` is the old side's file;
/// when empty no gap can be expanded and no controls are shown.
pub fn build_rows_expanded(hunks: &[Hunk], old_lines: &[String], expand: &[Expansion]) -> Vec<Row> {
    let spans = if old_lines.is_empty() { Vec::new() } else { spans(hunks, old_lines.len() as u32) };
    let last = hunks.len();
    let gap = |i: usize| {
        spans.get(i).map(|sp| {
            let (down, up, ctl) = cut(i, last, sp.to - sp.from, expand.get(i).copied().unwrap_or_default());
            (sp, down, up, ctl)
        })
    };
    let mut rows = Vec::new();
    for (hunk_index, h) in hunks.iter().enumerate() {
        let g = gap(hunk_index);
        if let Some((sp, down, _, _)) = g {
            push_context(&mut rows, old_lines, sp, sp.from, sp.from + down);
        }
        rows.push(Row::HunkHeader { hunk_index, header: h.header.clone(), gap: g.and_then(|x| x.3) });
        if let Some((sp, _, up, _)) = g {
            push_context(&mut rows, old_lines, sp, sp.to - up, sp.to);
        }
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
    if let Some((sp, down, _, ctl)) = gap(last) {
        push_context(&mut rows, old_lines, sp, sp.from, sp.from + down);
        if let Some(c) = ctl {
            rows.push(Row::Gap(c));
        }
    }
    rows
}
