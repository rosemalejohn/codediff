//! Measures ms from the last of 50 file writes to the Status event listing them.
//! Usage: cargo run --release --example refresh_bench -- <repo-path>
use codediff::controller::{Command, Controller, Event};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

fn main() {
    let repo = std::path::PathBuf::from(std::env::args().nth(1).expect("repo path"));
    let (tx, rx) = channel();
    let recent = std::env::temp_dir().join("codediff-bench-recent.json");
    let controller = Controller::spawn(recent, move |ev| {
        let _ = tx.send((Instant::now(), ev));
    });
    controller.send(Command::OpenRepo(repo.clone()));
    std::thread::sleep(Duration::from_secs(2));
    while rx.try_recv().is_ok() {}

    let dir = repo.join("bench_tmp");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..50 {
        std::fs::write(dir.join(format!("f{i}.txt")), format!("line {i}\n")).unwrap();
    }
    let last_write = Instant::now();
    let deadline = last_write + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok((t, ev)) = rx.recv_timeout(Duration::from_millis(50)) {
            if let Event::Status(s) = ev {
                let n = s
                    .unstaged
                    .iter()
                    .filter(|e| e.path.starts_with("bench_tmp/"))
                    .count();
                if n >= 50 {
                    println!("last write to Status with {n} files: {} ms", (t - last_write).as_millis());
                    break;
                }
            }
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
