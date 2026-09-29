//! About: the app's icon, name and description, "◇ By VesperX" with the
//! version, then links to vesperx.dk and the repository.

use super::MainWindow;
use adw::prelude::*;
use gtk::gio;
use std::f64::consts::{FRAC_PI_4, SQRT_2};
use std::rc::Rc;

const WEBSITE: &str = env!("CARGO_PKG_HOMEPAGE");
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const COPYRIGHT: &str = "© 2026 VesperX ApS";
const COMMENTS: &str =
    "Run several Signal accounts side by side, each with its own messages and app menu entry.";
/// The VesperX mark's box, in pixels; the brand's minimum for the mark alone is 16.
const MARK_SIZE: i32 = 18;

pub struct AboutDialog {
    pub dialog: adw::Dialog,
    pub icon: gtk::Image,
    pub byline: gtk::Label,
    pub mark: gtk::DrawingArea,
    pub version: gtk::Label,
    pub copyright: gtk::Label,
    links: gtk::ListBox,
}

impl AboutDialog {
    /// Each link row's title and the address it opens.
    pub fn links(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut row = self.links.first_child();
        while let Some(r) = row {
            let title = super::labels_in(&r)
                .first()
                .map(|l| l.text().to_string())
                .unwrap_or_default();
            out.push((title, r.tooltip_text().unwrap_or_default().to_string()));
            row = r.next_sibling();
        }
        out
    }
}

/// Builds and presents About over the main window.
pub fn present(win: &Rc<MainWindow>, app_name: &str) -> AboutDialog {
    let icon = gtk::Image::from_icon_name("system-users");
    icon.set_pixel_size(96);
    icon.add_css_class("about-icon");

    let name = gtk::Label::builder()
        .label(app_name)
        .css_classes(["about-name"])
        .build();

    let mark = vesperx_mark();
    let byline = gtk::Label::builder()
        .label("By VesperX")
        .css_classes(["about-byline"])
        .build();
    let version = gtk::Label::builder()
        .label(env!("CARGO_PKG_VERSION"))
        .css_classes(["about-version"])
        .build();
    let credit = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    credit.set_halign(gtk::Align::Center);
    credit.append(&mark);
    credit.append(&byline);
    credit.append(&version);

    let comments = gtk::Label::builder()
        .label(COMMENTS)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .max_width_chars(36)
        .css_classes(["dialog-body"])
        .build();

    let links = gtk::ListBox::new();
    links.set_selection_mode(gtk::SelectionMode::None);
    links.add_css_class("link-choices");
    for (title, uri) in [
        ("Website", WEBSITE.to_string()),
        ("Report an Issue", format!("{REPOSITORY}/issues")),
        ("Source Code", REPOSITORY.to_string()),
    ] {
        links.append(&link_row(title, &uri));
    }
    links.connect_row_activated({
        let win = Rc::downgrade(win);
        move |_, row| {
            let (Some(uri), Some(win)) = (row.tooltip_text(), win.upgrade()) else {
                return;
            };
            open_uri(&win, &uri);
        }
    });

    let copyright = gtk::Label::builder()
        .label(COPYRIGHT)
        .css_classes(["about-copyright"])
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.add_css_class("about-content");
    content.append(&icon);
    content.append(&name);
    content.append(&credit);
    content.append(&comments);
    content.append(&links);
    content.append(&copyright);

    let header = adw::HeaderBar::builder().show_title(false).build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&content));

    let dialog = adw::Dialog::builder()
        .title(format!("About {app_name}"))
        .content_width(360)
        .child(&view)
        .build();
    dialog.add_css_class("desktop-dialog");
    dialog.add_css_class("about-dialog");
    dialog.present(Some(&win.window));

    AboutDialog {
        dialog,
        icon,
        byline,
        mark,
        version,
        copyright,
        links,
    }
}

/// A row that opens `uri` in the browser; the tooltip shows where it goes.
fn link_row(title: &str, uri: &str) -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .hexpand(true)
        .build();
    let arrow = gtk::Image::from_icon_name("adw-external-link-symbolic");
    arrow.add_css_class("about-link-arrow");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.append(&label);
    content.append(&arrow);
    gtk::ListBoxRow::builder()
        .child(&content)
        .activatable(true)
        .tooltip_text(uri)
        .build()
}

fn open_uri(win: &Rc<MainWindow>, uri: &str) {
    let weak = Rc::downgrade(win);
    gtk::UriLauncher::new(uri).launch(Some(&win.window), gio::Cancellable::NONE, move |result| {
        if let (Err(e), Some(win)) = (result, weak.upgrade()) {
            win.toast(&format!("Could not open the link: {}", e.message()));
        }
    });
}

/// The VesperX mark, drawn rather than embedded so it stays sharp at any
/// scale: a bordered diamond around a small filled one, both rotated squares.
/// Its colour comes from the `vesperx-mark` CSS class.
fn vesperx_mark() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(MARK_SIZE)
        .content_height(MARK_SIZE)
        .valign(gtk::Align::Center)
        .css_classes(["vesperx-mark"])
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    area.set_draw_func(|area, cr, width, height| {
        let color = area.color();
        cr.set_source_rgba(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
            color.alpha().into(),
        );
        let (outer, inner, stroke) = mark_geometry(width.min(height).into());
        cr.translate(f64::from(width) / 2.0, f64::from(height) / 2.0);
        cr.rotate(FRAC_PI_4);
        cr.set_line_width(stroke);
        cr.rectangle(-outer / 2.0, -outer / 2.0, outer, outer);
        // A failed cairo call only leaves the mark undrawn; there is nothing to recover.
        let _ = cr.stroke();
        cr.rectangle(-inner / 2.0, -inner / 2.0, inner, inner);
        let _ = cr.fill();
    });
    area
}

/// Side of the outer and inner squares and the border width for a mark whose
/// rotated outline fits a `size`-pixel box. The inner square is 0.27 of the
/// outer one, as in the VesperX logo.
fn mark_geometry(size: f64) -> (f64, f64, f64) {
    let stroke = 1.5;
    let outer = (size - stroke) / SQRT_2;
    (outer, outer * 0.27, stroke)
}

#[cfg(test)]
mod tests {
    use super::mark_geometry;
    use std::f64::consts::SQRT_2;

    #[test]
    fn mark_fits_its_box_with_the_logo_proportions() {
        let (outer, inner, stroke) = mark_geometry(18.0);
        assert!(((outer * SQRT_2 + stroke) - 18.0).abs() < 1e-9);
        assert!((inner / outer - 0.27).abs() < 1e-9);
    }
}
