//! Background worker that owns the repository and turns Commands into Events.
use crate::branch::{self, BranchInfo, HeadInfo};
use crate::diff::{diff_stats, file_diff, files_in_target};
use crate::flow::{self, FlowConfig, FlowKind, FlowState};
use crate::flow_finish::{self, FinishPlan, FinishRequest, PushSpec};
use crate::highlight::{Highlighter, StyledRow};
use crate::history::list_commits;
use crate::model::{
    file_fingerprint, hunk_fingerprint, ChangeSide, CommitInfo, DiffBody, DiffTarget, FileDiff, FileEntry, Hunk,
    StatusLists,
};
use crate::recent::RecentRepos;
use crate::repo::{open_repo, repo_name};
use crate::sbs::{build_rows_expanded, ExpandDir, Expansion};
use crate::push;
use crate::stage;
use crate::status::list_status;
use crate::watch::RepoWatcher;
use crate::{CoreError, Result};
use git2::{Oid, Repository};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;

const HISTORY_PAGE: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    None,
    Change {
        side: ChangeSide,
        path: String,
    },
    Commit {
        id: Oid,
        path: Option<String>,
    },
    Range {
        base: Oid,
        head: Oid,
        path: Option<String>,
        /// The range ends at HEAD: the worker sets `head` to the current HEAD on
        /// select and on every refresh (the UI's `head` is only a hint).
        follows_head: bool,
    },
}

#[derive(Debug, Clone)]
pub enum Command {
    OpenRepo(PathBuf),
    Refresh,
    Select(Selection),
    LoadFullDiff,
    /// Reveal unchanged lines of gap `gap` (see `sbs::Expansion`) in the shown diff.
    ExpandGap {
        gap: usize,
        dir: ExpandDir,
    },
    LoadMoreHistory,
    /// Emit `Event::Branches` for the branch switcher.
    ListBranches,
    /// Check out a branch by full reference name (`refs/heads/..` or `refs/remotes/..`).
    Checkout(String),
    StageFile(String),
    UnstageFile(String),
    /// `fingerprint` is `file_fingerprint` of the unstaged diff the user saw.
    DiscardFile {
        path: String,
        fingerprint: u64,
    },
    StageHunk {
        path: String,
        hunk_index: usize,
        header: String,
        fingerprint: u64,
    },
    UnstageHunk {
        path: String,
        hunk_index: usize,
        header: String,
        fingerprint: u64,
    },
    DiscardHunk {
        path: String,
        hunk_index: usize,
        header: String,
        fingerprint: u64,
    },
    SetDark(bool),
    FlowInit(FlowConfig),
    /// Replies `Event::FlowStartCheck`.
    FlowCheckStart {
        kind: FlowKind,
        name: String,
    },
    FlowStart {
        kind: FlowKind,
        name: String,
    },
    /// Replies `Event::FlowPlan`.
    FlowPlan(FinishRequest),
    FlowFinish(FinishRequest),
    /// Pushes the push kept after `Event::FlowPushFailed` again.
    FlowRetryPush,
    /// Forgets that push and reports the finish as not pushed.
    FlowDropPush,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffView {
    pub file: FileDiff,
    pub rows: Vec<StyledRow>,
    pub side: Option<ChangeSide>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeInfo {
    pub base: Oid,
    pub head: Oid,
    pub commit_count: usize,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone)]
pub enum Event {
    RepoOpened {
        name: String,
        path: PathBuf,
        recent: Vec<PathBuf>,
    },
    Status(StatusLists),
    History {
        commits: Vec<CommitInfo>,
        append: bool,
        new_ids: Vec<Oid>,
    },
    Files(Vec<FileEntry>),
    Range(Option<RangeInfo>),
    Diff(Option<DiffView>),
    /// What HEAD points at; emitted on open and whenever it changes.
    Head(HeadInfo),
    Branches(Vec<BranchInfo>),
    Error(String),
    /// Git Flow state; emitted on open and whenever it changes.
    Flow(FlowState),
    FlowStartCheck {
        kind: FlowKind,
        name: String,
        error: Option<String>,
    },
    FlowPlan {
        request: FinishRequest,
        plan: FinishPlan,
    },
    /// The local finish steps are done and the push is running.
    FlowPushing,
    /// Git's reason; the push is kept for `Command::FlowRetryPush`.
    FlowPushFailed(String),
    /// The flow dialog closes; non-empty text is the notice to show.
    FlowDone(String),
    /// The flow dialog stays open and shows this.
    FlowFailed(String),
}

/// Handle to the worker thread. Dropping it shuts the worker down.
pub struct Controller {
    tx: Sender<Command>,
    handle: Option<JoinHandle<()>>,
}

impl Controller {
    pub fn spawn(recent_file: PathBuf, emit: impl Fn(Event) + Send + 'static) -> Controller {
        let (tx, rx) = channel();
        let worker_tx = tx.clone();
        let handle = std::thread::Builder::new()
            .name("codediff-controller".into())
            .spawn(move || {
                let mut w = Worker {
                    tx: worker_tx,
                    emit: Box::new(emit),
                    recent_file,
                    hl: Highlighter::new(),
                    dark: false,
                    git: None,
                    open: None,
                    selection: Selection::None,
                    force_full: false,
                    expand: Vec::new(),
                    view: None,
                };
                w.run(rx);
            })
            .expect("spawn controller thread");
        Controller {
            tx,
            handle: Some(handle),
        }
    }

    /// Queues a command; never blocks on Git work.
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(h) = self.handle.take() {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
    }
}

/// State tied to one open repository.
struct OpenRepo {
    repo: Repository,
    _watcher: Option<RepoWatcher>,
    loaded: usize,
    seen: HashSet<Oid>,
    /// Last emitted HEAD, to emit `Event::Head` only on change.
    head: Option<HeadInfo>,
    /// Last emitted Git Flow state, to emit `Event::Flow` only on change.
    flow: Option<FlowState>,
    /// A finished release or hotfix whose push failed, kept for a retry.
    pending_push: Option<PendingPush>,
}

struct PendingPush {
    spec: PushSpec,
    /// Notice when the push is dropped.
    notice: String,
    /// Notice when the push succeeds.
    pushed_notice: String,
}

struct Worker {
    tx: Sender<Command>,
    emit: Box<dyn Fn(Event) + Send>,
    recent_file: PathBuf,
    hl: Highlighter,
    dark: bool,
    /// The installed `git`, looked up on the first repository open.
    git: Option<Option<PathBuf>>,
    open: Option<OpenRepo>,
    selection: Selection,
    force_full: bool,
    /// Expansion of each gap of the shown diff; reset when the selection changes.
    expand: Vec<Expansion>,
    view: Option<DiffView>,
}

/// A hunk command as the UI sent it.
struct HunkReq {
    path: String,
    idx: usize,
    header: String,
    fingerprint: u64,
}

#[derive(Clone, Copy)]
enum HunkOp {
    Stage,
    Unstage,
    Discard,
}

fn head_id(repo: &Repository) -> Option<Oid> {
    repo.head().ok()?.peel_to_commit().ok().map(|c| c.id())
}

fn range_info(repo: &Repository, base: Oid, head: Oid) -> Result<RangeInfo> {
    let mut walk = repo.revwalk()?;
    walk.push(head)?;
    walk.hide(base)?;
    let commit_count = walk.count();
    let (additions, deletions) = diff_stats(repo, &DiffTarget::Range { base, head })?;
    Ok(RangeInfo {
        base,
        head,
        commit_count,
        additions,
        deletions,
    })
}

impl Worker {
    fn run(&mut self, rx: Receiver<Command>) {
        let mut pending: Option<Command> = None;
        loop {
            let cmd = match pending.take() {
                Some(c) => c,
                None => match rx.recv() {
                    Ok(c) => c,
                    Err(_) => break,
                },
            };
            if matches!(cmd, Command::Refresh) {
                // Coalesce queued refreshes (watcher bursts) into one.
                while let Ok(next) = rx.try_recv() {
                    if !matches!(next, Command::Refresh) {
                        pending = Some(next);
                        break;
                    }
                }
            }
            if matches!(cmd, Command::Shutdown) {
                break;
            }
            self.handle(cmd);
        }
    }

    fn emit(&self, e: Event) {
        (self.emit)(e);
    }

    fn report<T>(&self, r: Result<T>) -> Option<T> {
        r.map_err(|e| self.emit(Event::Error(e.to_string()))).ok()
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::OpenRepo(p) => self.open_repo(p),
            Command::Refresh => self.refresh(),
            Command::Select(sel) => self.select(sel),
            Command::LoadFullDiff => {
                self.force_full = true;
                self.emit_diff();
            }
            Command::ExpandGap { gap, dir } => {
                if self.view.is_none() {
                    return;
                }
                if self.expand.len() <= gap {
                    self.expand.resize(gap + 1, Expansion::default());
                }
                self.expand[gap].apply(dir);
                self.emit_diff();
            }
            Command::LoadMoreHistory => self.load_more_history(),
            Command::ListBranches => {
                let Some(o) = &self.open else { return };
                if let Some(list) = self.report(branch::list_branches(&o.repo)) {
                    self.emit(Event::Branches(list));
                }
            }
            Command::Checkout(refname) => self.mutate(|r| branch::checkout(r, &refname)),
            Command::StageFile(p) => self.mutate(|r| stage::stage_file(r, &p)),
            Command::UnstageFile(p) => self.mutate(|r| stage::unstage_file(r, &p)),
            Command::DiscardFile { path, fingerprint } => self.discard_file(path, fingerprint),
            Command::StageHunk {
                path,
                hunk_index,
                header,
                fingerprint,
            } => self.hunk(
                HunkOp::Stage,
                HunkReq {
                    path,
                    idx: hunk_index,
                    header,
                    fingerprint,
                },
            ),
            Command::UnstageHunk {
                path,
                hunk_index,
                header,
                fingerprint,
            } => self.hunk(
                HunkOp::Unstage,
                HunkReq {
                    path,
                    idx: hunk_index,
                    header,
                    fingerprint,
                },
            ),
            Command::DiscardHunk {
                path,
                hunk_index,
                header,
                fingerprint,
            } => self.hunk(
                HunkOp::Discard,
                HunkReq {
                    path,
                    idx: hunk_index,
                    header,
                    fingerprint,
                },
            ),
            Command::SetDark(dark) => {
                self.dark = dark;
                if self.view.is_some() {
                    self.emit_diff();
                }
            }
            Command::FlowInit(cfg) => self.flow_init(cfg),
            Command::FlowCheckStart { kind, name } => self.flow_check_start(kind, name),
            Command::FlowStart { kind, name } => self.flow_start(kind, name),
            Command::FlowPlan(req) => self.flow_plan(req),
            Command::FlowFinish(req) => self.flow_finish(req),
            Command::FlowRetryPush => self.flow_push(),
            Command::FlowDropPush => self.flow_drop_push(),
            Command::Shutdown => {}
        }
    }

    fn open_repo(&mut self, path: PathBuf) {
        let Some(repo) = self.report(open_repo(&path)) else {
            return;
        };
        let root: PathBuf = repo.workdir().unwrap_or(&path).components().collect();
        let tx = self.tx.clone();
        let watcher = self.report(RepoWatcher::start(root.clone(), move || {
            let _ = tx.send(Command::Refresh);
        }));
        if self.git.is_none() {
            self.git = Some(push::find_git());
        }
        let mut recent = RecentRepos::load_from(&self.recent_file);
        recent.touch(root.clone());
        let _ = recent.save_to(&self.recent_file);
        self.open = Some(OpenRepo {
            repo,
            _watcher: watcher,
            loaded: 0,
            seen: HashSet::new(),
            head: None,
            flow: None,
            pending_push: None,
        });
        self.selection = Selection::None;
        self.force_full = false;
        self.expand.clear();
        self.view = None;
        let name = repo_name(&self.open.as_ref().unwrap().repo);
        self.emit(Event::RepoOpened {
            name,
            path: root,
            recent: recent.paths,
        });
        self.emit_head();
        self.emit_flow();
        self.emit_status();
        self.emit_history_head(true);
        self.emit(Event::Diff(None));
    }

    /// Emits `Event::Head` when HEAD differs from the last one emitted.
    fn emit_head(&mut self) {
        let Some(o) = &mut self.open else { return };
        let head = match branch::head_info(&o.repo) {
            Ok(h) => h,
            Err(e) => return self.emit(Event::Error(e.to_string())),
        };
        if o.head.as_ref() != Some(&head) {
            o.head = Some(head.clone());
            self.emit(Event::Head(head));
        }
    }

    fn emit_status(&self) {
        let Some(o) = &self.open else { return };
        if let Some(s) = self.report(list_status(&o.repo)) {
            self.emit(Event::Status(s));
        }
    }

    /// Re-reads the loaded window of history from HEAD and emits it (append false).
    fn emit_history_head(&mut self, first: bool) {
        let Some(o) = &mut self.open else { return };
        let limit = o.loaded.max(HISTORY_PAGE);
        let commits = match list_commits(&o.repo, 0, limit) {
            Ok(c) => c,
            Err(e) => return self.emit(Event::Error(e.to_string())),
        };
        let new_ids: Vec<Oid> = if first {
            Vec::new()
        } else {
            commits
                .iter()
                .map(|c| c.id)
                .filter(|id| !o.seen.contains(id))
                .collect()
        };
        o.seen.extend(commits.iter().map(|c| c.id));
        o.loaded = commits.len();
        self.emit(Event::History {
            commits,
            append: false,
            new_ids,
        });
    }

    fn load_more_history(&mut self) {
        let Some(o) = &mut self.open else { return };
        let commits = match list_commits(&o.repo, o.loaded, HISTORY_PAGE) {
            Ok(c) => c,
            Err(e) => return self.emit(Event::Error(e.to_string())),
        };
        o.seen.extend(commits.iter().map(|c| c.id));
        o.loaded += commits.len();
        self.emit(Event::History {
            commits,
            append: true,
            new_ids: Vec::new(),
        });
    }

    fn refresh(&mut self) {
        if self.open.is_none() {
            return;
        }
        self.emit_head();
        self.emit_flow();
        self.emit_status();
        self.emit_history_head(false);
        if self.follow_head() {
            self.emit_range_and_files();
        }
        if !matches!(self.selection, Selection::None) && !self.emit_diff() {
            // The selected file has no diff any more.
            self.view = None;
            match &mut self.selection {
                Selection::Commit { path, .. } | Selection::Range { path, .. } => *path = None,
                s => *s = Selection::None,
            }
        }
    }

    /// For a range that follows HEAD, moves its head to the current HEAD.
    /// Returns true when the head changed.
    fn follow_head(&mut self) -> bool {
        let Some(o) = &self.open else { return false };
        let Selection::Range {
            head,
            follows_head: true,
            ..
        } = &mut self.selection
        else {
            return false;
        };
        match head_id(&o.repo) {
            Some(now) if now != *head => {
                *head = now;
                true
            }
            _ => false,
        }
    }

    fn select(&mut self, sel: Selection) {
        let was_range = matches!(self.selection, Selection::Range { .. });
        self.selection = sel;
        self.force_full = false;
        self.expand.clear();
        let moved = self.follow_head();
        match &self.selection {
            Selection::Range { path: None, .. } => {
                let files = self.emit_range_and_files();
                self.auto_select_first(files);
            }
            Selection::Range { .. } if moved => {
                self.emit_range_and_files();
                self.emit_diff();
            }
            Selection::Range { base, head, .. } => {
                let (base, head) = (*base, *head);
                if let Some(o) = &self.open {
                    if let Some(info) = self.report(range_info(&o.repo, base, head)) {
                        self.emit(Event::Range(Some(info)));
                    }
                }
                self.emit_diff();
            }
            other => {
                let needs_files = matches!(other, Selection::Commit { path: None, .. });
                if was_range {
                    self.emit(Event::Range(None));
                }
                if needs_files {
                    let files = self.emit_files();
                    self.auto_select_first(files);
                } else {
                    self.emit_diff();
                }
            }
        }
    }

    fn target(&self) -> Option<(DiffTarget, Option<ChangeSide>, Option<String>)> {
        Some(match &self.selection {
            Selection::None => return None,
            Selection::Change {
                side: ChangeSide::Staged,
                path,
            } => (
                DiffTarget::Staged,
                Some(ChangeSide::Staged),
                Some(path.clone()),
            ),
            Selection::Change {
                side: ChangeSide::Unstaged,
                path,
            } => (
                DiffTarget::Unstaged,
                Some(ChangeSide::Unstaged),
                Some(path.clone()),
            ),
            Selection::Commit { id, path } => (DiffTarget::Commit(*id), None, path.clone()),
            Selection::Range { base, head, path, .. } => (
                DiffTarget::Range {
                    base: *base,
                    head: *head,
                },
                None,
                path.clone(),
            ),
        })
    }

    /// Files of the current Commit/Range selection, emitted; returns them.
    fn emit_files(&self) -> Vec<FileEntry> {
        let (Some(o), Some((target, _, _))) = (&self.open, self.target()) else {
            return Vec::new();
        };
        let files = self
            .report(files_in_target(&o.repo, &target))
            .unwrap_or_default();
        self.emit(Event::Files(files.clone()));
        files
    }

    fn emit_range_and_files(&mut self) -> Vec<FileEntry> {
        let Selection::Range { base, head, .. } = self.selection else {
            return Vec::new();
        };
        let Some(o) = &self.open else {
            return Vec::new();
        };
        let info = self.report(range_info(&o.repo, base, head));
        self.emit(Event::Range(info));
        self.emit_files()
    }

    /// Selects the first file of the current Commit/Range listing and emits its diff.
    fn auto_select_first(&mut self, files: Vec<FileEntry>) {
        let first = files.into_iter().next().map(|f| f.path);
        match &mut self.selection {
            Selection::Commit { path, .. } | Selection::Range { path, .. } => *path = first,
            _ => return,
        }
        self.emit_diff();
    }

    /// Emits the diff of the current selection. Returns false when the selected
    /// path has no diff (Diff(None) emitted in that case too).
    fn emit_diff(&mut self) -> bool {
        // Gap indexes shift when another file shows or the hunk count changes.
        let hunks = |v: &Option<DiffView>| match v.as_ref().map(|v| (&v.file.path, &v.file.body)) {
            Some((p, DiffBody::Text(h))) => Some((p.clone(), h.len())),
            _ => None,
        };
        let before = hunks(&self.view);
        let mut built = self.build_view();
        if !self.expand.is_empty() && hunks(&built) != before {
            self.expand.clear();
            built = self.build_view();
        }
        let ok = built.is_some();
        self.view = built.clone();
        self.emit(Event::Diff(built));
        ok
    }

    fn build_view(&self) -> Option<DiffView> {
        let o = self.open.as_ref()?;
        let (target, side, path) = self.target()?;
        let path = path?;
        let file = self.report(file_diff(&o.repo, &target, &path, self.force_full))??;
        let rows = match &file.body {
            DiffBody::Text(hunks) => {
                let rows = build_rows_expanded(hunks, &file.old_lines, &self.expand);
                self.hl.highlight_rows(&path, &rows, &file.old_lines, self.dark)
            }
            _ => Vec::new(),
        };
        Some(DiffView { file, rows, side })
    }

    fn mutate(&mut self, op: impl FnOnce(&Repository) -> Result<()>) {
        let Some(o) = &self.open else { return };
        self.report(op(&o.repo));
        self.refresh();
    }

    /// The hunk the user saw, if the request still matches the current view.
    fn seen_hunk(&self, op: HunkOp, req: &HunkReq) -> Option<Hunk> {
        let v = self.view.as_ref()?;
        let side_ok = match op {
            HunkOp::Stage | HunkOp::Discard => v.side == Some(ChangeSide::Unstaged),
            HunkOp::Unstage => v.side == Some(ChangeSide::Staged),
        };
        let DiffBody::Text(hunks) = &v.file.body else {
            return None;
        };
        let h = hunks.get(req.idx)?;
        let same = v.file.path == req.path
            && h.header == req.header
            && hunk_fingerprint(h) == req.fingerprint;
        (side_ok && same).then(|| h.clone())
    }

    /// Discards the file only when its unstaged diff, recomputed now, still has
    /// the fingerprint of the diff the user confirmed.
    fn discard_file(&mut self, path: String, fingerprint: u64) {
        let Some(o) = &self.open else { return };
        let current = match file_diff(&o.repo, &DiffTarget::Unstaged, &path, self.force_full) {
            Ok(f) => f,
            Err(e) => {
                self.emit(Event::Error(e.to_string()));
                return self.refresh();
            }
        };
        if current.as_ref().map(file_fingerprint) != Some(fingerprint) {
            self.emit(Event::Error(CoreError::Stale.to_string()));
            return self.refresh();
        }
        self.mutate(|r| stage::discard_file(r, &path));
    }

    fn hunk(&mut self, op: HunkOp, req: HunkReq) {
        let Some(expected) = self.seen_hunk(op, &req) else {
            self.emit(Event::Error(CoreError::Stale.to_string()));
            return self.refresh();
        };
        let (path, idx) = (req.path, req.idx);
        self.mutate(|r| match op {
            HunkOp::Stage => stage::stage_hunk(r, &path, idx, &expected),
            HunkOp::Unstage => stage::unstage_hunk(r, &path, idx, &expected),
            HunkOp::Discard => stage::discard_hunk(r, &path, idx, &expected),
        });
    }

    // Git Flow

    /// Emits `Event::Flow` when the state differs from the last one emitted.
    fn emit_flow(&mut self) {
        let git_found = matches!(self.git, Some(Some(_)));
        let Some(o) = &mut self.open else { return };
        let state = match flow::flow_state(&o.repo, git_found) {
            Ok(s) => s,
            Err(e) => return self.emit(Event::Error(e.to_string())),
        };
        if o.flow.as_ref() != Some(&state) {
            o.flow = Some(state.clone());
            self.emit(Event::Flow(state));
        }
    }

    /// The open repository and its Git Flow settings; None when no
    /// repository is open or Git Flow is not initialized.
    fn flow_config(&self) -> Option<(&Repository, FlowConfig)> {
        let o = self.open.as_ref()?;
        let cfg = self.report(flow::load_config(&o.repo))??;
        Some((&o.repo, cfg))
    }

    fn flow_init(&mut self, cfg: FlowConfig) {
        let Some(o) = &self.open else { return };
        match flow::init(&o.repo, &cfg) {
            Ok(()) => {
                self.refresh();
                self.emit(Event::FlowDone(String::new()));
            }
            Err(CoreError::CheckoutConflict) => {
                // Initialization stands; only the switch to develop failed.
                self.refresh();
                self.emit(Event::FlowDone(String::new()));
                self.emit(Event::Error(CoreError::CheckoutConflict.to_string()));
            }
            Err(e) => self.emit(Event::FlowFailed(e.to_string())),
        }
    }

    fn flow_check_start(&self, kind: FlowKind, name: String) {
        let Some((repo, cfg)) = self.flow_config() else { return };
        let error = flow::start_error(repo, &cfg, kind, &name);
        self.emit(Event::FlowStartCheck { kind, name, error });
    }

    fn flow_start(&mut self, kind: FlowKind, name: String) {
        let Some((repo, cfg)) = self.flow_config() else { return };
        match flow::start(repo, &cfg, kind, &name) {
            Ok(()) => {
                self.refresh();
                self.emit(Event::FlowDone(String::new()));
            }
            Err(e) => self.emit(Event::FlowFailed(e.to_string())),
        }
    }

    fn flow_plan(&self, request: FinishRequest) {
        let Some((repo, cfg)) = self.flow_config() else { return };
        if let Some(plan) = self.report(flow_finish::plan_finish(repo, &cfg, &request)) {
            self.emit(Event::FlowPlan { request, plan });
        }
    }

    fn flow_finish(&mut self, req: FinishRequest) {
        // A new finish replaces any pending push.
        if let Some(o) = &mut self.open {
            o.pending_push = None;
        }
        let Some((repo, cfg)) = self.flow_config() else { return };
        let outcome = match flow_finish::finish(repo, &cfg, &req) {
            Ok(o) => o,
            Err(e) => {
                self.emit(Event::FlowFailed(e.to_string()));
                return self.flow_plan(req);
            }
        };
        if req.kind != FlowKind::Feature {
            self.report(flow::set_push_default(repo, req.push));
        }
        let Some(o) = &mut self.open else { return };
        o.pending_push = outcome.push.map(|spec| PendingPush {
            spec,
            notice: outcome.notice.clone(),
            pushed_notice: outcome.pushed_notice,
        });
        self.refresh();
        if self.has_pending_push() {
            self.flow_push();
        } else {
            self.emit(Event::FlowDone(outcome.notice));
        }
    }

    fn has_pending_push(&self) -> bool {
        self.open.as_ref().is_some_and(|o| o.pending_push.is_some())
    }

    /// Runs the pending push on this thread. Success forgets it; failure keeps it.
    fn flow_push(&mut self) {
        let Some(o) = &self.open else { return };
        let Some(p) = &o.pending_push else { return };
        self.emit(Event::FlowPushing);
        let result = match (&self.git, o.repo.workdir()) {
            (Some(Some(git)), Some(workdir)) => push::push(git, workdir, &p.spec),
            (Some(Some(_)), None) => Err("Repository has no working tree.".into()),
            _ => Err("Git command line tools not found.".into()),
        };
        match result {
            Ok(()) => {
                let done = self.open.as_mut().and_then(|o| o.pending_push.take());
                self.emit(Event::FlowDone(done.map(|p| p.pushed_notice).unwrap_or_default()));
            }
            Err(reason) => self.emit(Event::FlowPushFailed(reason)),
        }
    }

    fn flow_drop_push(&mut self) {
        let Some(o) = &mut self.open else { return };
        if let Some(p) = o.pending_push.take() {
            self.emit(Event::FlowDone(p.notice));
        }
    }
}
