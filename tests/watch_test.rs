mod common;
use codediff::watch::{is_relevant, is_relevant_in, RepoWatcher};
use common::TestRepo;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn never(_: &Path) -> bool {
    false
}

#[test]
fn git_internal_noise_ignored() {
    let root = Path::new("/r");
    for p in ["/r/.git/objects/ab/cd", "/r/.git/index.lock", "/r/.git/logs/HEAD"] {
        assert!(!is_relevant(root, Path::new(p), &never), "{p}");
    }
    for p in ["/r/.git/index", "/r/.git/HEAD", "/r/.git/refs/heads/main", "/r/.git/packed-refs"] {
        assert!(is_relevant(root, Path::new(p), &never), "{p}");
    }
}

#[test]
fn linked_worktree_git_dirs_are_relevant() {
    // Workdir /wt (its .git is a file), gitdir /m/.git/worktrees/wt, commondir /m/.git.
    let root = Path::new("/wt");
    let dirs = [
        Path::new("/m/.git/worktrees/wt").to_path_buf(),
        Path::new("/m/.git").to_path_buf(),
    ];
    for p in [
        "/m/.git/worktrees/wt/index",
        "/m/.git/worktrees/wt/HEAD",
        "/m/.git/refs/heads/wt",
        "/m/.git/packed-refs",
        "/wt/src/a.rs",
    ] {
        assert!(is_relevant_in(root, &dirs, Path::new(p), &never), "{p}");
    }
    for p in [
        "/m/.git/worktrees/wt/index.lock",
        "/m/.git/worktrees/wt/logs/HEAD",
        "/m/.git/objects/ab/cd",
        "/m/.git/worktrees/other/HEAD",
        "/wt/.git",
        "/m/src/a.rs",
    ] {
        assert!(!is_relevant_in(root, &dirs, Path::new(p), &never), "{p}");
    }
}

#[test]
fn ignored_paths_not_relevant() {
    let root = Path::new("/r");
    let ign = |p: &Path| p.starts_with("node_modules");
    assert!(!is_relevant(root, Path::new("/r/node_modules/x.js"), &ign));
    assert!(is_relevant(root, Path::new("/r/src/main.rs"), &ign));
}

#[test]
fn outside_root_not_relevant() {
    assert!(!is_relevant(Path::new("/r"), Path::new("/other/a.rs"), &never));
}

fn counter() -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let c = Arc::new(AtomicUsize::new(0));
    (c.clone(), c)
}

#[test]
fn burst_of_writes_fires_few_callbacks() {
    let t = TestRepo::new();
    let (c, c2) = counter();
    let _w = RepoWatcher::start(t.path().to_path_buf(), move || {
        c2.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    for i in 0..50 {
        t.write(&format!("f{i}.txt"), b"x");
    }
    std::thread::sleep(Duration::from_millis(1000));
    let n = c.load(Ordering::SeqCst);
    assert!((1..=3).contains(&n), "count {n}");
}

#[test]
fn ignored_dir_writes_do_not_fire() {
    let t = TestRepo::new();
    t.write(".gitignore", b"node_modules/\n");
    t.stage(".gitignore");
    let (c, c2) = counter();
    std::thread::sleep(Duration::from_millis(300));
    let _w = RepoWatcher::start(t.path().to_path_buf(), move || {
        c2.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    for i in 0..20 {
        t.write(&format!("node_modules/f{i}.js"), b"x");
    }
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(c.load(Ordering::SeqCst), 0);
}
