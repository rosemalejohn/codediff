//! Opens Start New Feature from the real Git Flow menu with Slint's software
//! renderer and pointer clicks, then types: the name field must have the
//! keyboard focus once the menu popup has closed.
//!
//! Element lookups need Slint debug info, which the build leaves out, so the
//! Git Flow button is found by hovering along the top bar: the repository,
//! branch and Git Flow buttons each tint their background under the pointer.

use codediff::controller::{Command, Event};
use codediff::flow::{FlowConfig, FlowKind, FlowState};
use codediff::ui::{AppWindow, Flow};
use codediff::ui_bridge;
use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, SharedString};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

const W: usize = 1200;
const H: usize = 800;
// Top bar: 44px tall, buttons centered on y 22, 28 or 30px tall.
const BAR_MID: f32 = 22.0;
// Inside every button, above its icon and label.
const PROBE_Y: usize = 10;

struct Offscreen {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

fn click(window: &MinimalSoftwareWindow, position: LogicalPosition) {
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    slint::platform::update_timers_and_animations();
    window.draw_if_needed(|_| {});
}

fn type_text(window: &MinimalSoftwareWindow, text: &str) {
    for c in text.chars() {
        let text = SharedString::from(c.to_string());
        window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        window.dispatch_event(WindowEvent::KeyReleased { text });
    }
}

/// Initialized, on `feature/login-form`.
fn flow_state() -> FlowState {
    let config = FlowConfig {
        production: "main".into(),
        develop: "develop".into(),
        feature: "feature/".into(),
        release: "release/".into(),
        hotfix: "hotfix/".into(),
        version_tag: String::new(),
    };
    FlowState {
        config: Some(config.clone()),
        draft: config,
        init_blocked: None,
        branches: vec!["develop".into(), "feature/login-form".into(), "main".into()],
        features: vec!["feature/login-form".into()],
        releases: vec![],
        hotfixes: vec![],
        current: Some((FlowKind::Feature, "feature/login-form".into())),
        latest_tag: None,
        push_blocked: None,
        push_default: false,
    }
}

/// Renders the window into `buf` (only what changed is repainted).
fn render(window: &MinimalSoftwareWindow, buf: &mut [PremultipliedRgbaColor]) {
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(buf, W);
    });
}

fn same(a: PremultipliedRgbaColor, b: PremultipliedRgbaColor) -> bool {
    (a.red, a.green, a.blue) == (b.red, b.green, b.blue)
}

/// Left and right edge of each top-bar button, from left to right.
fn bar_buttons(window: &MinimalSoftwareWindow) -> Vec<(f32, f32)> {
    let mut buf = vec![PremultipliedRgbaColor::default(); W * H];
    let away = LogicalPosition::new(W as f32 / 2.0, H as f32 / 2.0);
    window.dispatch_event(WindowEvent::PointerMoved { position: away });
    render(window, &mut buf);
    let bar = buf[PROBE_Y * W + W / 2];
    let mut runs: Vec<(f32, f32)> = Vec::new();
    for x in (0..W / 2).step_by(2) {
        window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(x as f32, BAR_MID) });
        render(window, &mut buf);
        if same(buf[PROBE_Y * W + x], bar) {
            continue;
        }
        match runs.last_mut() {
            Some(run) if x as f32 - run.1 <= 2.0 => run.1 = x as f32,
            _ => runs.push((x as f32, x as f32)),
        }
    }
    window.dispatch_event(WindowEvent::PointerMoved { position: away });
    runs
}

#[test]
fn start_feature_from_the_menu_focuses_the_name_field() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Offscreen { window: window.clone(), start: Instant::now() }))
        .unwrap();
    window.set_size(PhysicalSize::new(W as u32, H as u32));

    let app = AppWindow::new().unwrap();
    let log = Rc::new(RefCell::new(Vec::<Command>::new()));
    let l = log.clone();
    ui_bridge::wire(&app, move |c| l.borrow_mut().push(c));
    app.show().unwrap();
    ui_bridge::apply(
        &app,
        Event::RepoOpened { name: "proj".into(), path: PathBuf::from("/w/proj"), recent: vec![] },
    );
    ui_bridge::apply(&app, Event::Flow(flow_state()));
    window.draw_if_needed(|_| {});

    let buttons = bar_buttons(&window);
    assert!(buttons.len() >= 3, "top bar buttons not found: {buttons:?}");
    let (left, right) = buttons[2];
    click(&window, LogicalPosition::new((left + right) / 2.0, BAR_MID));
    // The menu opens 6px under the 28px button (top at y 8): 5px padding,
    // Initialize and Next Action (30px each), a 9px separator, then
    // Start New Feature.
    click(&window, LogicalPosition::new(left + 60.0, 8.0 + 28.0 + 6.0 + 5.0 + 30.0 + 30.0 + 9.0 + 15.0));
    let flow = app.global::<Flow>();
    assert_eq!((flow.get_dialog(), flow.get_kind()), (2, 0), "Start New Feature did not open");

    type_text(&window, "login");
    assert_eq!(flow.get_name(), "login");
    assert!(matches!(
        log.borrow().last(),
        Some(Command::FlowCheckStart { kind: FlowKind::Feature, name }) if name == "login"
    ));
}
