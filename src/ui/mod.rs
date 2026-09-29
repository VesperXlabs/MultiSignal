//! The GTK user interface. It renders what the store reports and forwards
//! clicks to it; every rule lives in the GTK-free modules.

pub mod about_dialog;
mod app;
mod avatar;
pub mod create_dialog;
pub mod delete_dialog;
mod detail;
mod install_page;
pub mod link_dialog;
pub mod link_handler;
mod settings;
mod sidebar_row;
mod window;

use crate::store::Store;
use crate::system::Installer;
use crate::units;
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

pub use app::application;
pub use window::MainWindow;

/// Everything the UI needs from the outside world, so tests can pass fakes.
pub struct Deps {
    pub store: Store,
    pub installer: Arc<dyn Installer>,
}

pub fn build_window(deps: Deps) -> Rc<MainWindow> {
    MainWindow::new(deps)
}

/// "3 profiles · 1 running · 208 MB" (the running part only when non-zero).
pub fn summary_line(count: usize, running: usize, bytes: u64) -> String {
    let noun = if count == 1 { "profile" } else { "profiles" };
    let mut parts = vec![format!("{count} {noun}")];
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    parts.push(units::size(bytes));
    parts.join(" · ")
}

const STYLE: &str = include_str!("style.css");
const STYLE_LIGHT: &str = include_str!("style-light.css");
const STYLE_DARK: &str = include_str!("style-dark.css");

/// Loads the shared stylesheet plus the light or dark colour tokens, and swaps
/// the tokens when the system switches between light and dark.
pub fn load_css() {
    let display = gtk::gdk::Display::default().expect("a display is available");
    let base = gtk::CssProvider::new();
    base.load_from_string(STYLE);
    let tokens = gtk::CssProvider::new();
    let manager = adw::StyleManager::default();
    let load_tokens = {
        let tokens = tokens.clone();
        move |m: &adw::StyleManager| {
            tokens.load_from_string(if m.is_dark() { STYLE_DARK } else { STYLE_LIGHT })
        }
    };
    load_tokens(&manager);
    manager.connect_dark_notify(load_tokens);
    for provider in [&tokens, &base] {
        gtk::style_context_add_provider_for_display(
            &display,
            provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// Parse errors in the bundled stylesheets, for tests.
pub fn css_errors() -> Vec<String> {
    let errors = Rc::new(RefCell::new(Vec::new()));
    for (file, css) in [
        ("style.css", STYLE),
        ("style-light.css", STYLE_LIGHT),
        ("style-dark.css", STYLE_DARK),
    ] {
        let provider = gtk::CssProvider::new();
        let sink = errors.clone();
        provider.connect_parsing_error(move |_, section, error| {
            sink.borrow_mut()
                .push(format!("{file}: {section}: {}", error.message()));
        });
        provider.load_from_string(css);
    }
    errors.take()
}

/// A macOS-style push button: `classes` such as "suggested-action",
/// optional key hint ("esc", "↵") after the label.
fn push_button(label: &str, classes: &[&str], key_hint: Option<&str>) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("push");
    for class in classes {
        button.add_css_class(class);
    }
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.set_halign(gtk::Align::Center);
    content.append(&gtk::Label::new(Some(label)));
    if let Some(hint) = key_hint {
        let hint = gtk::Label::new(Some(hint));
        hint.add_css_class("key-hint");
        content.append(&hint);
    }
    button.set_child(Some(&content));
    button
}

/// The text of a push button made by `push_button` (its first label).
fn button_label(button: &gtk::Button) -> String {
    labels_in(button)
        .first()
        .map(|l| l.text().to_string())
        .unwrap_or_default()
}

/// Changes the text of a push button made by `push_button`.
fn set_button_label(button: &gtk::Button, text: &str) {
    if let Some(label) = labels_in(button).first() {
        label.set_text(text);
    }
}

/// Every label below `widget`, in tree order.
fn labels_in(widget: &impl IsA<gtk::Widget>) -> Vec<gtk::Label> {
    let mut out = Vec::new();
    let mut child = widget.as_ref().first_child();
    while let Some(c) = child {
        if let Some(label) = c.downcast_ref::<gtk::Label>() {
            out.push(label.clone());
        }
        out.extend(labels_in(&c));
        child = c.next_sibling();
    }
    out
}

/// The shared desktop-dialog layout: icon and title/body side by side, an
/// extra area lined up under the title, then right-aligned buttons.
struct DialogFrame {
    dialog: adw::Dialog,
    title: gtk::Label,
    body: gtk::Label,
    extra: gtk::Box,
    buttons: gtk::Box,
}

fn dialog_frame(icon: &impl IsA<gtk::Widget>, title: &str, body: &str, width: i32) -> DialogFrame {
    let dialog = adw::Dialog::builder().content_width(width).build();
    dialog.add_css_class("desktop-dialog");

    let title = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dialog-title"])
        .build();
    let body = gtk::Label::builder()
        .label(body)
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(40)
        .css_classes(["dialog-body"])
        .build();
    let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
    text.set_hexpand(true);
    text.append(&title);
    text.append(&body);

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    icon.as_ref().set_valign(gtk::Align::Start);
    head.append(icon);
    head.append(&text);

    let extra = gtk::Box::new(gtk::Orientation::Vertical, 6);
    extra.set_margin_start(68);
    extra.set_visible(false);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.add_css_class("dialog-content");
    content.append(&head);
    content.append(&extra);
    content.append(&buttons);
    dialog.set_child(Some(&content));

    DialogFrame {
        dialog,
        title,
        body,
        extra,
        buttons,
    }
}

#[cfg(test)]
mod tests {
    use super::summary_line;

    #[test]
    fn summary_line_reads_naturally() {
        assert_eq!(summary_line(1, 0, 20_000), "1 profile · 20 KB");
        assert_eq!(
            summary_line(3, 1, 208_000_000),
            "3 profiles · 1 running · 208 MB"
        );
    }
}
