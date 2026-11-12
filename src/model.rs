//! Core data types shared across the library.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

impl FileStatus {
    pub fn letter(&self) -> char {
        match self {
            FileStatus::Added | FileStatus::Untracked => 'A',
            FileStatus::Modified => 'M',
            FileStatus::Deleted => 'D',
            FileStatus::Renamed => 'R',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLists {
    pub staged: Vec<FileEntry>,
    pub unstaged: Vec<FileEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeSide {
    Staged,
    Unstaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffTarget {
    Unstaged,
    Staged,
    Commit(git2::Oid),
    /// Tree of `base` versus tree of `head`; `base` own changes are excluded.
    Range { base: git2::Oid, head: git2::Oid },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffBody {
    Text(Vec<Hunk>),
    Binary { old_size: u64, new_size: u64 },
    TooLarge { lines: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub body: DiffBody,
    pub additions: u32,
    pub deletions: u32,
    /// Lines of the old side's file (Text bodies only; empty when there is no
    /// old side). Source of the unchanged lines shown when context is expanded.
    pub old_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub id: git2::Oid,
    /// First 7 characters of the id.
    pub short_id: String,
    pub summary: String,
    pub author: String,
    /// Unix seconds.
    pub time: i64,
}

/// Content fingerprint of a hunk (header plus every line's kind and text).
/// Stable within one process only; used to check a hunk command still targets
/// exactly what the user saw.
pub fn hunk_fingerprint(h: &Hunk) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut s = DefaultHasher::new();
    h.header.hash(&mut s);
    for l in &h.lines {
        (l.kind as u8).hash(&mut s);
        l.text.hash(&mut s);
    }
    s.finish()
}

/// Content fingerprint of a whole file diff: status and body kind, plus every
/// hunk fingerprint (Text), both sizes (Binary) or the line count (TooLarge).
/// Same stability as `hunk_fingerprint`; used to check a file-level discard
/// still targets what the user saw.
pub fn file_fingerprint(f: &FileDiff) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut s = DefaultHasher::new();
    (f.status as u8).hash(&mut s);
    match &f.body {
        DiffBody::Text(hunks) => {
            0u8.hash(&mut s);
            hunks.len().hash(&mut s);
            for h in hunks {
                hunk_fingerprint(h).hash(&mut s);
            }
        }
        DiffBody::Binary { old_size, new_size } => {
            1u8.hash(&mut s);
            old_size.hash(&mut s);
            new_size.hash(&mut s);
        }
        DiffBody::TooLarge { lines } => {
            2u8.hash(&mut s);
            lines.hash(&mut s);
        }
    }
    s.finish()
}
