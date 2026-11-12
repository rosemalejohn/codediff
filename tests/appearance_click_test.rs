//! Clicks through the real appearance menu with Slint's software renderer.
//! Regression: menu items used to close the popup before applying the choice,
//! which tore the item down so the choice never ran.

use codediff::settings::{Appearance, Settings};
use codediff::ui::AppWindow;
use codediff::ui_bridge;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize};
use std::rc::Rc;
use std::time::{Duration, Instant};

const W: f32 = 1200.0;

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

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    slint::platform::update_timers_and_animations();
}

/// Opens the menu from the top-bar icon and clicks item `i` (0 System, 1 Light, 2 Dark).
fn pick(window: &MinimalSoftwareWindow, i: f32) {
    click(window, W - 12.0 - 14.0, 20.0);
    // Popup under the button: 4px padding, 28px items, 2px apart.
    click(window, W - 100.0, 37.0 + 4.0 + 14.0 + i * 30.0);
}

#[test]
fn choosing_from_the_menu_applies_and_saves() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen { window: window.clone(), start: Instant::now() }))
        .unwrap();
    window.set_size(PhysicalSize::new(W as u32, 800));

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.json");
    let app = AppWindow::new().unwrap();
    ui_bridge::set_appearance(&app, Appearance::Dark);
    ui_bridge::wire_appearance(&app, file.clone());
    app.show().unwrap();
    window.draw_if_needed(|_| {});

    pick(&window, 1.0);
    assert_eq!(app.get_appearance(), 1, "Light not applied");
    assert!(!app.get_dark());
    assert_eq!(Settings::load_from(&file).appearance, Appearance::Light);

    pick(&window, 2.0);
    assert_eq!(app.get_appearance(), 2, "Dark not applied");
    assert!(app.get_dark());
    assert_eq!(Settings::load_from(&file).appearance, Appearance::Dark);
}
