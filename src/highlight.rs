//! Syntax highlighting of side-by-side rows.
use crate::model::LineKind;
use crate::sbs::{Cell, Row};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

pub struct Highlighter {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StyledSeg {
    pub text: String,
    pub rgb: (u8, u8, u8),
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StyledCell {
    pub no: u32,
    pub kind: LineKind,
    pub segs: Vec<StyledSeg>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StyledRow {
    HunkHeader { hunk_index: usize, header: String },
    Line { left: Option<StyledCell>, right: Option<StyledCell> },
}

impl Highlighter {
    /// Loads syntaxes and themes. Slow: construct once per app.
    pub fn new() -> Self {
        Highlighter {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            themes: ThemeSet::load_defaults(),
        }
    }

    pub fn highlight_rows(&self, path: &str, rows: &[Row], dark: bool) -> Vec<StyledRow> {
        let theme_name = if dark { "base16-ocean.dark" } else { "InspiredGitHub" };
        let theme = &self.themes.themes[theme_name];
        let syntax = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .and_then(|e| self.syntaxes.find_syntax_by_extension(e))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text());
        let mut left = HighlightLines::new(syntax, theme);
        let mut right = HighlightLines::new(syntax, theme);
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            match row {
                Row::HunkHeader { hunk_index, header } => {
                    left = HighlightLines::new(syntax, theme);
                    right = HighlightLines::new(syntax, theme);
                    out.push(StyledRow::HunkHeader { hunk_index: *hunk_index, header: header.clone() });
                }
                Row::Line { left: l, right: r } => {
                    let l = l.as_ref().map(|c| self.style_cell(&mut left, c));
                    let r = r.as_ref().map(|c| self.style_cell(&mut right, c));
                    out.push(StyledRow::Line { left: l, right: r });
                }
            }
        }
        out
    }

    fn style_cell(&self, hl: &mut HighlightLines, cell: &Cell) -> StyledCell {
        let text: String = cell.segs.iter().map(|s| s.text.as_str()).collect();
        let mut line = text.clone();
        line.push('\n');
        // On error, fall back to a single unstyled span so state stays usable.
        let spans = hl.highlight_line(&line, &self.syntaxes).unwrap_or_default();

        // Changed ranges as (start, end, changed) over `text`.
        let mut ranges = Vec::with_capacity(cell.segs.len());
        let mut pos = 0;
        for s in &cell.segs {
            ranges.push((pos, pos + s.text.len(), s.changed));
            pos += s.text.len();
        }

        let mut segs: Vec<StyledSeg> = Vec::new();
        let mut off = 0;
        for (style, piece) in spans {
            let (s0, s1) = (off, off + piece.len());
            off = s1;
            let rgb = (style.foreground.r, style.foreground.g, style.foreground.b);
            for &(r0, r1, changed) in &ranges {
                let (a, b) = (s0.max(r0), s1.min(r1));
                if a < b {
                    push_styled(&mut segs, &text[a..b], rgb, changed);
                }
            }
        }
        // Highlighting failed or lost text: keep the text intact.
        let covered: usize = segs.iter().map(|s| s.text.len()).sum();
        if covered != text.len() {
            segs = cell
                .segs
                .iter()
                .filter(|s| !s.text.is_empty())
                .map(|s| StyledSeg { text: s.text.clone(), rgb: (0, 0, 0), changed: s.changed })
                .collect();
        }
        StyledCell { no: cell.no, kind: cell.kind, segs }
    }
}

fn push_styled(segs: &mut Vec<StyledSeg>, text: &str, rgb: (u8, u8, u8), changed: bool) {
    if let Some(last) = segs.last_mut() {
        if last.rgb == rgb && last.changed == changed {
            last.text.push_str(text);
            return;
        }
    }
    segs.push(StyledSeg { text: text.to_string(), rgb, changed });
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DiffLine, Hunk};
    use crate::sbs::build_rows;

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
        DiffLine { kind, old_no: old, new_no: new, text: text.to_string() }
    }

    fn hunk(lines: Vec<DiffLine>) -> Hunk {
        Hunk { header: "@@ -1 +1 @@".into(), old_start: 1, old_lines: 1, new_start: 1, new_lines: 1, lines }
    }

    fn cells(rows: &[StyledRow]) -> Vec<&StyledCell> {
        let mut out = Vec::new();
        for r in rows {
            if let StyledRow::Line { left, right } = r {
                out.extend(left.iter());
                out.extend(right.iter());
            }
        }
        out
    }

    #[test]
    fn rust_keyword_gets_color() {
        let rows = build_rows(&[hunk(vec![line(LineKind::Context, Some(1), Some(1), "fn main() {}")])]);
        let out = Highlighter::new().highlight_rows("x.rs", &rows, true);
        let c = cells(&out)[0];
        let fn_seg = c.segs.iter().find(|s| s.text == "fn").expect("fn seg");
        let main_seg = c.segs.iter().find(|s| s.text == "main").expect("main seg");
        assert_ne!(fn_seg.rgb, main_seg.rgb);
        let joined: String = c.segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "fn main() {}");
    }

    #[test]
    fn unknown_extension_plain_text() {
        let rows = build_rows(&[hunk(vec![line(LineKind::Context, Some(1), Some(1), "fn main() {}")])]);
        let out = Highlighter::new().highlight_rows("x.zzzunknown", &rows, false);
        for c in cells(&out) {
            assert_eq!(c.segs.len(), 1);
            assert_eq!(c.segs[0].text, "fn main() {}");
        }
    }

    #[test]
    fn changed_flag_survives_split() {
        let rows = build_rows(&[hunk(vec![
            line(LineKind::Removed, Some(1), None, "let value = 1;"),
            line(LineKind::Added, None, Some(1), "let value = 2;"),
        ])]);
        let out = Highlighter::new().highlight_rows("x.rs", &rows, true);
        let right = match &out[1] {
            StyledRow::Line { right: Some(r), .. } => r,
            _ => panic!("expected line"),
        };
        assert!(right.segs.iter().any(|s| s.changed && s.text.contains('2')));
        assert!(right.segs.iter().any(|s| !s.changed));
        let joined: String = right.segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "let value = 2;");
    }

    #[test]
    fn empty_cell_does_not_panic() {
        let rows = build_rows(&[hunk(vec![
            line(LineKind::Removed, Some(1), None, ""),
            line(LineKind::Added, None, Some(1), "x"),
        ])]);
        let out = Highlighter::new().highlight_rows("x.rs", &rows, true);
        assert_eq!(out.len(), rows.len());
    }
}
