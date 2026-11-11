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
