//! Floating AI prompt behavior, driven through the headless egui UI.

use egui::{Event, Key, Modifiers};
use figcraft_engine::Session;
use serde_json::json;

use crate::layers_drag_tests::Ui;

fn cmd() -> Modifiers {
    if crate::shortcuts::platform() == figcraft_engine::keymap::Platform::Mac {
        Modifiers::MAC_CMD | Modifiers::COMMAND
    } else {
        Modifiers::CTRL | Modifiers::COMMAND
    }
}

#[test]
fn ai_command_opens_prompt_instead_of_preferences() {
    let mut u = Ui::new(Session::new());
    assert!(!u.app.ui.ai.open);
    u.app.run("ai.open", json!({}));
    assert!(u.app.ui.ai.open);
    assert!(!u.app.ui.preferences);
    u.app.run("ai.open", json!({}));
    assert!(!u.app.ui.ai.open, "the toolbar command toggles the bar");
}

#[test]
fn command_j_opens_prompt_on_the_canvas() {
    let mut u = Ui::new(Session::new());
    let modifiers = cmd();
    let event = |pressed| Event::Key { key: Key::J, physical_key: None, pressed, repeat: false, modifiers };
    u.frame_with(vec![event(true), event(false)], modifiers);
    assert!(u.app.ui.ai.open);
}

#[test]
fn enter_submits_the_focused_prompt() {
    let mut u = Ui::new(Session::new());
    u.app.ui.ai.open = true;
    u.app.ui.ai.prompt = "A dashboard".into();
    u.frame(vec![]);
    let event = |pressed| Event::Key { key: Key::Enter, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE };
    u.frame(vec![event(true), event(false)]);
    assert!(u.app.ui.preferences, "submitting with no provider opens its optional configuration");
}
