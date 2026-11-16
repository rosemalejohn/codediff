//! Renders the real CodeDiff UI offscreen and writes a BMP, for UI review
//! without screen-capture permission.
//!
//! Usage: cargo run --example screenshot -- <repo> <out.bmp> <appearance 0|1|2> [menu|branches]
//! Convert with: sips -s format png out.bmp --out out.png

use codediff::controller::Controller;
use codediff::settings::Appearance;
use codediff::ui::AppWindow;
use codediff::ui_bridge;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const W: u32 = 1200;
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
    let commit = args.get(4).and_then(|s| s.strip_prefix("commit=")).map(str::to_string);

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(OffscreenPlatform { window: window.clone(), start: Instant::now() }))
        .unwrap();
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: SCALE });
    window.set_size(PhysicalSize::new((W as f32 * SCALE) as u32, (H as f32 * SCALE) as u32));

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

    if open_menu {
        click(&window, W as f32 - 12.0 - 14.0, 20.0);
        pump(&app, &rx, 300);
    }

    // `pick=<0|1|2>`: open the appearance menu and click that item.
    if let Some(i) = args.get(4).and_then(|s| s.strip_prefix("pick=")).and_then(|s| s.parse::<f32>().ok()) {
        click(&window, W as f32 - 12.0 - 14.0, 20.0);
        pump(&app, &rx, 300);
        // Popup sits under the button: items 28px tall, 2px apart, 4px padding.
        click(&window, W as f32 - 100.0, 37.0 + 4.0 + 14.0 + i * 30.0);
        pump(&app, &rx, 300);
        println!("appearance after pick: {}  dark: {}", app.get_appearance(), app.get_dark());
    }

    let (pw, ph) = ((W as f32 * SCALE) as u32, (H as f32 * SCALE) as u32);
    let mut buf = vec![PremultipliedRgbaColor::default(); (pw * ph) as usize];
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(&mut buf, pw as usize);
    });
    write_bmp(&out, pw, ph, &buf).unwrap();
    println!("wrote {out}");
}
