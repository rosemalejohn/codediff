//! Renders the real CodeDiff UI offscreen and writes a BMP, for UI review
//! without screen-capture permission.
//!
//! Usage: cargo run --example screenshot -- <repo> <out.bmp> <appearance 0|1|2> [mode] [width=<px>]
//! Modes: menu, branches, commit=<id>, pick=<0|1|2>, and the Git Flow screens
//! flow-menu, flow-init, flow-start, flow-start-release, flow-next, flow-finish,
//! flow-finish-release, flow-blocked, flow-conflict, flow-pushing, flow-push-failed,
//! flow-done. The Git Flow modes set the `Flow` global directly, so any repository works.
//! Convert with: sips -s format png out.bmp --out out.png

use codediff::controller::Controller;
use codediff::flow::{FlowConfig, FlowKind, FlowState, TagInfo};
use codediff::settings::Appearance;
use codediff::ui::{AppWindow, Flow, FlowStep};
use codediff::ui_bridge;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, SharedString, VecModel};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEFAULT_W: u32 = 1200;
const H: u32 = 800;
const SCALE: f32 = 2.0;

struct OffscreenPlatform {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}

impl Platform for OffscreenPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

fn pump(app: &AppWindow, rx: &mpsc::Receiver<codediff::controller::Event>, for_ms: u64) {
    let until = Instant::now() + Duration::from_millis(for_ms);
    while Instant::now() < until {
        while let Ok(ev) = rx.try_recv() {
            ui_bridge::apply(app, ev);
        }
        slint::platform::update_timers_and_animations();
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
}

fn write_bmp(path: &str, w: u32, h: u32, px: &[PremultipliedRgbaColor]) -> std::io::Result<()> {
    let data_len = w * h * 4;
    let mut out = Vec::with_capacity(54 + data_len as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + data_len).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes()); // top-down rows
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for p in px {
        out.extend_from_slice(&[p.blue, p.green, p.red, 255]);
    }
    std::fs::write(path, out)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let repo = PathBuf::from(&args[1]);
    let out = args[2].clone();
    let appearance = Appearance::from_index(args[3].parse().unwrap_or(0));
    let open_menu = args.get(4).map(|s| s == "menu").unwrap_or(false);
    // `commit=<short id>`: show that commit in the History tab instead.
    let flow_mode = args.get(4).filter(|m| m.starts_with("flow-")).cloned();
    let width: u32 = args
        .iter()
        .skip(4)
        .find_map(|a| a.strip_prefix("width=").and_then(|w| w.parse().ok()))
        .unwrap_or(DEFAULT_W);
    let commit = args.get(4).and_then(|s| s.strip_prefix("commit=")).map(str::to_string);

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(OffscreenPlatform { window: window.clone(), start: Instant::now() }))
        .unwrap();
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: SCALE });
    window.set_size(PhysicalSize::new((width as f32 * SCALE) as u32, (H as f32 * SCALE) as u32));

    let app = AppWindow::new().unwrap();
    ui_bridge::set_appearance(&app, appearance);
    app.show().unwrap();

    // Real controller against a throwaway recent-repos file.
    let tmp = std::env::temp_dir().join("codediff-screenshot-recent.json");
    let (tx, rx) = mpsc::channel();
    let controller = Rc::new(Controller::spawn(tmp, move |ev| {
        let _ = tx.send(ev);
    }));
    let c = controller.clone();
    ui_bridge::wire(&app, move |cmd| c.send(cmd));
    ui_bridge::wire_appearance(&app, std::env::temp_dir().join("codediff-screenshot-settings.json"));
    controller.send(codediff::controller::Command::OpenRepo(repo));
    pump(&app, &rx, 1500);

    // Select the first unstaged file so the diff pane has content.
    use slint::Model;
    if let Some(row) = app.get_unstaged().row_data(0) {
        app.invoke_select_change(1, row.path.clone());
        pump(&app, &rx, 1500);
    }

    if let Some(short) = commit {
        app.set_active_tab(1);
        pump(&app, &rx, 300);
        let commits = app.get_commits();
        if let Some(row) = (0..commits.row_count())
            .filter_map(|i| commits.row_data(i))
            .find(|r| r.id.starts_with(&short))
        {
            app.invoke_select_commit(row.id.clone());
            pump(&app, &rx, 1500);
        }
    }

    // `branches`: open the Cmd+B branch switcher.
    if args.get(4).map(String::as_str) == Some("branches") {
        app.set_switcher_mode(1);
        app.set_switcher_open(true);
        app.invoke_list_branches();
        pump(&app, &rx, 500);
    }

    if let Some(mode) = flow_mode {
        flow_screen(&app, &window, &mode, width as f32);
    }

    if open_menu {
        click(&window, width as f32 - 12.0 - 14.0, 20.0);
        pump(&app, &rx, 300);
    }

    // `pick=<0|1|2>`: open the appearance menu and click that item.
    if let Some(i) = args.get(4).and_then(|s| s.strip_prefix("pick=")).and_then(|s| s.parse::<f32>().ok()) {
        click(&window, width as f32 - 12.0 - 14.0, 20.0);
        pump(&app, &rx, 300);
        // Popup sits under the button: items 28px tall, 2px apart, 4px padding.
        click(&window, width as f32 - 100.0, 37.0 + 4.0 + 14.0 + i * 30.0);
        pump(&app, &rx, 300);
        println!("appearance after pick: {}  dark: {}", app.get_appearance(), app.get_dark());
    }

    let (pw, ph) = ((width as f32 * SCALE) as u32, (H as f32 * SCALE) as u32);
    let mut buf = vec![PremultipliedRgbaColor::default(); (pw * ph) as usize];
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(&mut buf, pw as usize);
    });
    write_bmp(&out, pw, ph, &buf).unwrap();
    println!("wrote {out}");
}

fn text(s: &str) -> SharedString {
    s.into()
}

fn flow_config() -> FlowConfig {
    FlowConfig {
        production: "main".into(),
        develop: "develop".into(),
        feature: "feature/".into(),
        release: "release/".into(),
        hotfix: "hotfix/".into(),
        version_tag: String::new(),
    }
}

/// The example data of the mockups: `main`, `develop`, one open feature, latest tag 1.1.0.
fn flow_state(initialized: bool, current: Option<(FlowKind, &str)>, release: bool) -> FlowState {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    FlowState {
        config: initialized.then(flow_config),
        draft: flow_config(),
        init_blocked: None,
        branches: vec!["develop".into(), "feature/login-form".into(), "main".into()],
        features: if initialized { vec!["feature/login-form".into()] } else { vec![] },
        releases: if release { vec!["release/1.2.0".into()] } else { vec![] },
        hotfixes: vec![],
        current: current.map(|(k, b)| (k, b.to_string())),
        latest_tag: Some(TagInfo { name: "1.1.0".into(), time: now - 21 * 86400 }),
        push_blocked: None,
        push_default: false,
    }
}

fn steps(items: &[(&str, &str, bool, i32)]) -> ModelRc<FlowStep> {
    let rows: Vec<FlowStep> = items
        .iter()
        .map(|(t, n, remote, state)| FlowStep { text: text(t), note: text(n), remote: *remote, state: *state })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

/// Left and right edge of the top-bar button at `index`, found by hovering along
/// the bar: each button tints its background under the pointer.
fn bar_button(window: &MinimalSoftwareWindow, width: f32, index: usize) -> (f32, f32) {
    let (pw, ph) = ((width * SCALE) as usize, (H as f32 * SCALE) as usize);
    let mut buf = vec![PremultipliedRgbaColor::default(); pw * ph];
    let grab = |window: &MinimalSoftwareWindow, buf: &mut [PremultipliedRgbaColor]| {
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(buf, pw);
        });
    };
    let probe = |buf: &[PremultipliedRgbaColor], x: f32| {
        let p = buf[(10.0 * SCALE) as usize * pw + (x * SCALE) as usize];
        (p.red, p.green, p.blue)
    };
    window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(width / 2.0, H as f32 / 2.0) });
    grab(window, &mut buf);
    let bar = probe(&buf, width / 2.0);
    let mut runs: Vec<(f32, f32)> = Vec::new();
    let mut x = 0.0;
    while x < width / 2.0 {
        window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(x, 22.0) });
        grab(window, &mut buf);
        if probe(&buf, x) != bar {
            match runs.last_mut() {
                Some(run) if x - run.1 <= 2.0 => run.1 = x,
                _ => runs.push((x, x)),
            }
        }
        x += 2.0;
    }
    window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(width / 2.0, H as f32 / 2.0) });
    runs[index]
}

/// Puts the `Flow` global in the state of one Git Flow screen. Worker events are
/// not applied afterwards, so the real repository cannot overwrite it.
fn flow_screen(app: &AppWindow, window: &MinimalSoftwareWindow, mode: &str, width: f32) {
    let f = app.global::<Flow>();
    let feature = Some((FlowKind::Feature, "feature/login-form"));
    let release = Some((FlowKind::Release, "release/1.2.0"));
    let state = match mode {
        "flow-menu" | "flow-start" | "flow-start-release" | "flow-done" => flow_state(true, None, false),
        "flow-init" => flow_state(false, None, false),
        "flow-next" | "flow-finish" | "flow-blocked" | "flow-conflict" => flow_state(true, feature, false),
        _ => flow_state(true, release, true),
    };
    ui_bridge::apply(app, codediff::controller::Event::Flow(state));
    let branch = match mode {
        "flow-next" | "flow-finish" | "flow-blocked" | "flow-conflict" => "feature/login-form",
        "flow-finish-release" | "flow-pushing" | "flow-push-failed" => "release/1.2.0",
        _ => "develop",
    };
    app.set_branch_name(branch.into());
    let local = [
        ("Merge release/1.2.0 into main", "5 commits", false, 0),
        ("Tag main as 1.2.0", "", false, 0),
        ("Merge release/1.2.0 back into develop", "", false, 0),
        ("Delete release/1.2.0", "", false, 0),
        ("Switch to develop", "", false, 0),
    ];
    let feature_steps = [
        ("Merge feature/login-form into develop", "3 commits", false, 0),
        ("Delete feature/login-form", "", false, 0),
        ("Switch to develop", "", false, 0),
    ];
    let release_steps = [
        local[0],
        local[1],
        local[2],
        local[3],
        local[4],
        ("Push main, develop and tag 1.2.0 to origin", "", true, 0),
        ("Delete release/1.2.0 on origin", "", true, 0),
    ];
    let finish_feature = |app: &AppWindow| {
        let f = app.global::<Flow>();
        f.set_dialog(3);
        f.set_kind(0);
        f.set_branch(text("feature/login-form"));
        f.set_branch_options(ModelRc::new(VecModel::from(vec![text("feature/login-form")])));
        f.set_steps(steps(&feature_steps));
        f.set_delete_branch(true);
        f.set_blocker_level(0);
    };
    let finish_release = |app: &AppWindow| {
        let f = app.global::<Flow>();
        f.set_dialog(3);
        f.set_kind(1);
        f.set_branch(text("release/1.2.0"));
        f.set_branch_options(ModelRc::new(VecModel::from(vec![text("release/1.2.0")])));
        f.set_tag(text("1.2.0"));
        f.set_message(text("Release 1.2.0"));
        f.set_delete_branch(true);
        f.set_push(true);
        f.set_blocker_level(0);
    };
    match mode {
        "flow-menu" => {
            let (left, right) = bar_button(window, width, 2);
            window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(0.0, 400.0) });
            click(window, (left + right) / 2.0, 22.0);
        }
        "flow-init" => {
            f.invoke_action(text("init"));
        }
        "flow-start" => {
            f.invoke_action(text("start:feature"));
            f.set_name(text("login-form"));
        }
        "flow-start-release" => {
            f.invoke_action(text("start:release"));
            f.set_name(text("1.2.0"));
        }
        "flow-next" => {
            app.set_switcher_mode(2);
            app.set_switcher_query(text(""));
            app.invoke_switcher_filter(text(""));
            app.set_switcher_open(true);
        }
        "flow-finish" => finish_feature(app),
        "flow-finish-release" => {
            finish_release(app);
            f.set_steps(steps(&release_steps));
        }
        "flow-blocked" => {
            finish_feature(app);
            f.set_blocker_level(1);
            f.set_blocker_title(text("13 uncommitted changes"));
            f.set_blocker_body(text(
                "Finish needs a clean working tree. Commit them, or discard them in Changes, then try again.",
            ));
        }
        "flow-conflict" => {
            finish_feature(app);
            f.set_blocker_level(2);
            f.set_blocker_title(text("Cannot merge cleanly"));
            f.set_blocker_body(text(
                "develop and feature/login-form both changed these files. Nothing was changed. \
                 Merge develop into feature/login-form first (agent or terminal), then finish again.",
            ));
            let files = vec![text("src/controller.rs"), text("ui/app.slint")];
            f.set_blocker_files(ModelRc::new(VecModel::from(files)));
        }
        "flow-pushing" => {
            finish_release(app);
            f.set_dialog(4);
            let mut rows = release_steps;
            for (i, row) in rows.iter_mut().enumerate() {
                row.3 = if i < 5 { 1 } else if i == 5 { 2 } else { 0 };
            }
            f.set_steps(steps(&rows));
        }
        "flow-push-failed" => {
            finish_release(app);
            f.set_dialog(5);
            f.set_push_reason(text(
                "origin rejected main: it has commits you do not have. Nothing was forced. Pull main, then retry.",
            ));
            let mut rows = release_steps;
            for (i, row) in rows.iter_mut().enumerate() {
                row.3 = if i < 5 { 1 } else { 3 };
            }
            f.set_steps(steps(&rows));
        }
        "flow-done" => {
            f.set_notice(text("Finished feature/login-form. Merged into develop and deleted the branch."));
        }
        other => panic!("unknown mode {other}"),
    }
    slint::platform::update_timers_and_animations();
}
