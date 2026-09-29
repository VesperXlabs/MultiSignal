//! The main window: toast overlay → stack of "install" | "empty" | "app". The
//! app page is a sidebar + detail split view that folds into list → detail
//! navigation below 720 px.

use super::create_dialog::{self, CreateDialog, Purpose};
use super::delete_dialog::{self, TrashDialog};
use super::detail::{self, DetailWidgets};
use super::install_page::InstallPage;
use super::link_dialog::{self, LinkDialog};
use super::link_handler;
use super::settings::{self, Appearance};
use super::{Deps, button_label, push_button, sidebar_row, summary_line};
use crate::links::{self, Route};
use crate::procs;
use crate::profiles::{DEFAULT_NAME, Profile};
use crate::store::{AdoptError, CreateError};
use crate::system::InstallOutcome;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

const APP_NAME: &str = "Signal Profiles";
/// How far the content must scroll before its bar shows a title and hairline.
const DETAIL_SCROLLED_AT: f64 = 110.0;
const LIST_SCROLLED_AT: f64 = 40.0;

pub struct MainWindow {
    pub window: adw::ApplicationWindow,
    pub(super) deps: Deps,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    install_page: InstallPage,
    split: adw::NavigationSplitView,
    sidebar: Sidebar,
    content: Content,
    context_menu: gtk::PopoverMenu,
    detail: RefCell<Option<DetailWidgets>>,
    profiles: RefCell<Vec<Profile>>,
    selected: RefCell<Option<String>>,
    installed: Cell<bool>,
    /// The button label of the last toast that had one, for tests.
    last_toast_button: RefCell<Option<String>>,
    /// The user's name for the default Signal, from the settings file; shown
    /// as "<name> (default)".
    default_title: RefCell<Option<String>>,
    /// Set while the list is rebuilt, so the selection signals it fires are ignored.
    rebuilding: Cell<bool>,
}

struct Sidebar {
    toolbar: adw::ToolbarView,
    /// Holds the list; the context menu hangs off it, not off the list,
    /// because `ListBox::remove_all` would try to remove it as a row.
    body: gtk::Box,
    bar: adw::HeaderBar,
    title: gtk::Label,
    more: gtk::MenuButton,
    scroller: gtk::ScrolledWindow,
    large_title: gtk::Box,
    large_summary: gtk::Label,
    list: gtk::ListBox,
    note: gtk::Label,
    footer: gtk::Label,
}

struct Content {
    page: adw::NavigationPage,
    bar: adw::HeaderBar,
    title: gtk::Label,
    back: gtk::Button,
    more: gtk::MenuButton,
    scroller: gtk::ScrolledWindow,
    clamp: adw::Clamp,
}

impl MainWindow {
    pub fn new(deps: Deps) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .title(APP_NAME)
            .default_width(1100)
            .default_height(720)
            .width_request(360)
            .height_request(480)
            .build();

        let installed = deps.installer.is_installed();
        let other = if installed {
            None
        } else {
            deps.installer.other_install()
        };
        let install_page = InstallPage::new(other);
        let (empty_page, empty_bar, empty_actions) = whole_window_page(
            "system-users-symbolic",
            "No Profiles Yet",
            "Each profile runs its own Signal account, with separate messages and its own app menu entry.",
        );
        empty_bar.pack_end(&new_profile_button());
        let create = push_button("Create Profile", &["suggested-action", "large"], None);
        create.set_action_name(Some("win.new-profile"));
        empty_actions.append(&create);

        let sidebar = build_sidebar();
        let content = build_content();
        let sidebar_page = adw::NavigationPage::builder()
            .title("Profiles")
            .child(&sidebar.toolbar)
            .build();
        let split = adw::NavigationSplitView::builder()
            .min_sidebar_width(250.0)
            .max_sidebar_width(320.0)
            .sidebar_width_fraction(0.26)
            .sidebar(&sidebar_page)
            .content(&content.page)
            .build();
        let narrow = adw::Breakpoint::new(
            adw::BreakpointCondition::parse("max-width: 720sp").expect("valid condition"),
        );
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(narrow);

        let stack = gtk::Stack::new();
        stack.add_named(&install_page.root, Some("install"));
        stack.add_named(&empty_page, Some("empty"));
        stack.add_named(&split, Some("app"));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));
        window.set_content(Some(&toasts));

        let context_menu = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
        context_menu.set_has_arrow(false);
        context_menu.set_halign(gtk::Align::Start);
        context_menu.set_parent(&sidebar.body);
        sidebar.body.connect_destroy({
            let menu = context_menu.clone();
            move |_| menu.unparent()
        });

        let default_title = settings::default_title(&deps.store.paths.settings);
        let this = Rc::new(Self {
            window,
            deps,
            toasts,
            stack,
            install_page,
            split,
            sidebar,
            content,
            context_menu,
            detail: RefCell::new(None),
            profiles: RefCell::new(Vec::new()),
            selected: RefCell::new(None),
            installed: Cell::new(installed),
            last_toast_button: RefCell::new(None),
            default_title: RefCell::new(default_title),
            rebuilding: Cell::new(false),
        });
        this.install_actions();
        this.connect_signals();
        this.restore_window_state();
        this.reload();
        this
    }

    /// Reads the profiles again and shows the right page. The selection is
    /// kept; if its profile is gone, the neighbour at the same position is
    /// selected instead.
    pub fn reload(&self) {
        if !self.installed.get() {
            self.installed.set(self.deps.installer.is_installed());
        }
        if !self.installed.get() {
            self.stack.set_visible_child_name("install");
            self.update_actions();
            return;
        }
        let mut profiles = match self.deps.store.load() {
            Ok(p) => p,
            Err(e) => {
                self.toast(&format!("Could not read your profiles: {e}"));
                return;
            }
        };
        if let Some(title) = self.default_title.borrow().as_ref() {
            // Keep the marker: it's the Signal set up without Signal Profiles.
            for p in profiles.iter_mut().filter(|p| p.is_default) {
                p.title = format!("{title} (default)");
            }
        }
        let selected = {
            let old = self.profiles.borrow();
            let current = self.selected.borrow();
            let still_there = current
                .as_ref()
                .filter(|s| profiles.iter().any(|p| &p.name == *s));
            let old_index = current
                .as_ref()
                .and_then(|s| old.iter().position(|p| &p.name == s));
            still_there
                .cloned()
                .or_else(|| {
                    let last = profiles.len().checked_sub(1)?;
                    Some(profiles[old_index?.min(last)].name.clone())
                })
                .or_else(|| profiles.first().map(|p| p.name.clone()))
        };
        *self.profiles.borrow_mut() = profiles;
        *self.selected.borrow_mut() = selected;
        if self.profiles.borrow().is_empty() {
            self.stack.set_visible_child_name("empty");
            self.update_actions();
            return;
        }
        // Show the page first: render() enables actions based on it.
        self.stack.set_visible_child_name("app");
        self.render();
    }

    /// Rebuilds the sidebar and detail from `profiles`, styled for the
    /// current layout (split or collapsed).
    fn render(&self) {
        let compact = self.split.is_collapsed();
        let s = &self.sidebar;
        {
            let profiles = self.profiles.borrow();
            self.rebuilding.set(true);
            s.list.remove_all();
            for p in profiles.iter() {
                s.list.append(&sidebar_row::build(p, compact));
            }
            if compact {
                s.list.set_selection_mode(gtk::SelectionMode::None);
            } else {
                s.list.set_selection_mode(gtk::SelectionMode::Single);
                if let Some(row) = self.selected.borrow().as_deref().and_then(|n| self.row(n)) {
                    s.list.select_row(Some(&row));
                }
            }
            self.rebuilding.set(false);

            let running = profiles.iter().filter(|p| p.running).count();
            let bytes = profiles.iter().map(|p| p.size_bytes).sum();
            let summary = summary_line(profiles.len(), running, bytes);
            s.footer.set_text(&summary);
            s.large_summary.set_text(&summary);
        }

        s.list.set_activate_on_single_click(compact);
        set_class(&s.list, "navigation-sidebar", !compact);
        set_class(&s.list, "boxed-list", compact);
        set_class(&s.list, "compact-list", compact);
        set_class(&s.toolbar, "compact-sidebar", compact);
        set_class(&s.title, "bar-title", compact);
        s.large_title.set_visible(compact);
        s.note.set_visible(compact);
        s.footer.set_visible(!compact);
        s.more.set_visible(compact);
        self.content.more.set_visible(!compact);
        self.content.back.set_visible(compact);
        self.render_detail();
    }

    fn render_detail(&self) {
        let compact = self.split.is_collapsed();
        let profile = self.selected_profile();
        let widgets = profile.as_ref().map(|p| detail::build(p, compact));
        self.content
            .clamp
            .set_child(widgets.as_ref().map(|d| &d.root));
        let title = profile.as_ref().map_or("", |p| p.title.as_str());
        self.content.title.set_text(title);
        self.content.page.set_title(title);
        *self.detail.borrow_mut() = widgets;
        self.update_actions();
    }

    fn show_profile(&self, name: &str) {
        let changed = self.selected.borrow().as_deref() != Some(name);
        *self.selected.borrow_mut() = Some(name.to_string());
        if changed {
            self.content.scroller.vadjustment().set_value(0.0);
        }
        self.render_detail();
    }

    /// Selects a profile as a click would: in the folded layout this also
    /// opens its detail page.
    pub fn select(&self, name: &str) {
        if self.split.is_collapsed() {
            self.show_profile(name);
            self.split.set_show_content(true);
        } else if let Some(row) = self.row(name) {
            self.sidebar.list.select_row(Some(&row));
        }
    }

    fn selected_profile(&self) -> Option<Profile> {
        let selected = self.selected.borrow();
        let name = selected.as_deref()?;
        self.profiles
            .borrow()
            .iter()
            .find(|p| p.name == name)
            .cloned()
    }

    fn row(&self, name: &str) -> Option<gtk::ListBoxRow> {
        rows(&self.sidebar.list).find(|r| r.widget_name() == name)
    }

    // Actions -------------------------------------------------------------

    fn install_actions(self: &Rc<Self>) {
        self.add_action("new-profile", |this| {
            this.open_create_dialog();
        });
        self.add_action("open-selected", |this| this.open_selected());
        self.add_action("show-selected", |this| this.show_selected_in_files());
        self.add_action("trash-selected", |this| {
            this.open_trash_dialog();
        });
        self.add_action("repair", |this| this.repair(true));
        self.add_action("repair-all", |this| this.repair(false));
        self.add_action("about", |this| this.show_about());
        self.add_action("lock-default", |this| this.set_default_locked(true));
        self.add_action("unlock-default", |this| this.set_default_locked(false));
        self.add_action("adopt-default", |this| {
            this.open_adopt_dialog();
        });

        // Only a saved choice is applied, so by default the app follows GNOME
        // without touching the style manager.
        let saved = Appearance::load(&self.deps.store.paths.settings);
        if let Some(appearance) = saved {
            appearance.apply();
        }
        let current = saved.unwrap_or(Appearance::System);
        let appearance = gio::SimpleAction::new_stateful(
            "appearance",
            Some(glib::VariantTy::STRING),
            &current.id().to_variant(),
        );
        let weak = Rc::downgrade(self);
        appearance.connect_activate(move |_, target| {
            let chosen = target
                .and_then(|t| t.get::<String>())
                .and_then(|id| Appearance::from_id(&id));
            if let (Some(this), Some(chosen)) = (weak.upgrade(), chosen) {
                this.set_appearance(chosen);
            }
        });
        self.window.add_action(&appearance);

        let handle_links = gio::SimpleAction::new_stateful(
            "handle-links",
            None,
            &link_handler::is_default().to_variant(),
        );
        let weak = Rc::downgrade(self);
        handle_links.connect_activate(move |action, _| {
            let Some(this) = weak.upgrade() else { return };
            let on = !action
                .state()
                .and_then(|s| s.get::<bool>())
                .unwrap_or(false);
            if let Err(e) = link_handler::set_default(on) {
                this.toast(&format!("Could not change the link handler: {e}"));
            }
            action.set_state(&link_handler::is_default().to_variant());
        });
        self.window.add_action(&handle_links);
    }

    fn add_action(self: &Rc<Self>, name: &str, run: impl Fn(&Rc<Self>) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                run(&this);
            }
        });
        self.window.add_action(&action);
    }

    /// Profile actions need a selection; Move to Trash also needs the
    /// profile to be stopped.
    fn update_actions(&self) {
        let profile = self
            .selected_profile()
            .filter(|_| self.stack.visible_child_name().as_deref() != Some("install"));
        let enable = |name: &str, on: bool| {
            if let Some(action) = self.window.lookup_action(name)
                && let Ok(action) = action.downcast::<gio::SimpleAction>()
            {
                action.set_enabled(on);
            }
        };
        enable("open-selected", profile.as_ref().is_some_and(|p| !p.locked));
        enable("show-selected", profile.is_some());
        let own = profile.as_ref().filter(|p| !p.is_default);
        enable("repair", own.is_some());
        enable("trash-selected", own.is_some_and(|p| !p.running));
        let stopped_default = profile
            .as_ref()
            .is_some_and(|p| p.is_default && !p.running && p.dir.is_dir());
        enable("adopt-default", stopped_default);
        let default = profile.as_ref().filter(|p| p.is_default);
        enable("lock-default", default.is_some_and(|p| !p.locked));
        enable("unlock-default", default.is_some_and(|p| p.locked));
    }

    pub fn open_create_dialog(self: &Rc<Self>) -> CreateDialog {
        create_dialog::present(self, Purpose::NewProfile, "")
    }

    /// "Move to a Profile" for the default Signal, pre-filled with the
    /// user's name for it when that's a valid profile name.
    pub fn open_adopt_dialog(self: &Rc<Self>) -> CreateDialog {
        let bytes = self
            .profiles
            .borrow()
            .iter()
            .find(|p| p.is_default)
            .map_or(0, |p| p.size_bytes);
        let initial = self
            .default_title
            .borrow()
            .clone()
            .filter(|t| crate::names::is_valid(t))
            .unwrap_or_default();
        create_dialog::present(self, Purpose::AdoptDefault(bytes), &initial)
    }

    /// Moves the default Signal's data into a profile, selects it, and
    /// forgets the default's display name (it now belongs to the profile).
    pub(super) fn adopt_default(self: &Rc<Self>, input: &str) -> Result<String, AdoptError> {
        let name = self.deps.store.adopt_default(input)?;
        let path = &self.deps.store.paths.settings;
        let file = settings::load(path);
        if file.has_group("default") {
            let _ = file.remove_group("default");
            if let Err(e) = settings::save(path, &file) {
                eprintln!("multisignal: could not update {}: {e}", path.display());
            }
        }
        *self.default_title.borrow_mut() = None;
        *self.selected.borrow_mut() = Some(name.clone());
        self.reload();
        let text = format!("The default Signal is now the profile “{name}”");
        if crate::lock::is_locked(&self.deps.store.paths) {
            self.toast(&text);
        } else {
            // It's empty now; locking keeps it from being set up again by accident.
            self.toast_with_button(&text, "Lock", |this| this.set_default_locked(true));
        }
        Ok(name)
    }

    /// Hides or shows the snap's Signal entry.
    fn set_default_locked(&self, locked: bool) {
        let (result, done) = if locked {
            (
                self.deps.store.lock_default(),
                "The Signal entry is hidden from your app menu",
            )
        } else {
            (
                self.deps.store.unlock_default(),
                "The Signal entry is back in your app menu",
            )
        };
        match result {
            Ok(()) => {
                self.reload();
                self.toast(done);
            }
            Err(e) => self.toast(&format!("Could not change the Signal entry: {e}")),
        }
    }

    /// Routes a Signal link: the one running profile gets it; otherwise a
    /// dialog asks which profile (returned, for tests).
    pub fn open_link(self: &Rc<Self>, link: &str) -> Option<LinkDialog> {
        if !links::is_signal_link(link) {
            self.toast(&format!("Not a Signal link: {link}"));
            return None;
        }
        self.reload(); // who is running right now
        let profiles = self.profiles.borrow().clone();
        match links::route(&profiles) {
            Route::To(name) => {
                self.deliver_link(&name, link);
                None
            }
            Route::Ask(names) if names.is_empty() => {
                self.toast("There is no Signal profile to open this link");
                None
            }
            Route::Ask(names) => {
                let candidates: Vec<Profile> = profiles
                    .into_iter()
                    .filter(|p| names.contains(&p.name))
                    .collect();
                Some(link_dialog::present(self, link, &candidates))
            }
        }
    }

    pub(super) fn deliver_link(&self, name: &str, link: &str) {
        let title = self
            .profiles
            .borrow()
            .iter()
            .find(|p| p.name == name)
            .map_or_else(|| name.to_string(), |p| p.title.clone());
        match self.deps.store.open_link(name, link) {
            Ok(()) => self.toast(&format!("Sent the link to Signal ({title})")),
            Err(e) => self.toast(&format!("Could not send the link to Signal ({title}): {e}")),
        }
    }

    pub fn open_trash_dialog(self: &Rc<Self>) -> Option<TrashDialog> {
        let profile = self.selected_profile().filter(|p| !p.is_default)?;
        if profile.running {
            self.toast(&crate::store::DeleteError::Running(profile.name).to_string());
            return None;
        }
        Some(delete_dialog::present(self, &profile))
    }

    /// Creates the profile, selects it and offers to open it.
    pub(super) fn create_profile(self: &Rc<Self>, input: &str) -> Result<String, CreateError> {
        let name = self.deps.store.create(input)?;
        *self.selected.borrow_mut() = Some(name.clone());
        self.reload();
        if self.split.is_collapsed() {
            self.split.set_show_content(true);
        }
        let launched = name.clone();
        self.toast_with_button(&format!("“{name}” created"), "Open", move |this| {
            this.launch(&launched)
        });
        Ok(name)
    }

    pub(super) fn trash_profile(&self, name: &str) {
        match self.deps.store.delete(name) {
            Ok(()) => {
                self.reload();
                self.split.set_show_content(false);
                self.toast(&format!("“{name}” moved to Trash"));
            }
            Err(e) => {
                self.reload();
                self.toast(&e.to_string());
            }
        }
    }

    fn open_selected(self: &Rc<Self>) {
        if let Some(p) = self.selected_profile() {
            self.launch(&p.name);
        }
    }

    /// Starts (or, if it's running, brings forward) Signal for a profile, and
    /// refreshes soon after so "Running" appears promptly.
    fn launch(self: &Rc<Self>, name: &str) {
        let launched = if name == DEFAULT_NAME {
            self.deps.store.launch_default()
        } else {
            self.deps.store.launch(name)
        };
        if let Err(e) = launched {
            let title = self
                .profiles
                .borrow()
                .iter()
                .find(|p| p.name == name)
                .map_or_else(|| name.to_string(), |p| p.title.clone());
            self.toast(&format!("Could not open Signal ({title}): {e}"));
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            if let Some(this) = weak.upgrade() {
                this.refresh_running();
            }
        });
    }

    fn show_selected_in_files(self: &Rc<Self>) {
        let Some(p) = self.selected_profile() else {
            return;
        };
        let weak = Rc::downgrade(self);
        gtk::FileLauncher::new(Some(&gio::File::for_path(&p.dir))).launch(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |result| {
                if let (Err(e), Some(this)) = (result, weak.upgrade()) {
                    this.toast(&format!("Could not open the folder: {}", e.message()));
                }
            },
        );
    }

    /// Repairs every launcher; the toast talks about the selected profile
    /// when started from its Repair button.
    fn repair(&self, for_selected: bool) {
        let report = match self.deps.store.repair() {
            Ok(r) => r,
            Err(e) => {
                self.toast(&format!("Could not repair app menu entries: {e}"));
                return;
            }
        };
        let text = match (for_selected, self.selected.borrow().clone()) {
            (true, Some(name)) if report.created.contains(&name) => {
                format!("Created app menu entry for “{name}”")
            }
            _ => match report.created.len() {
                0 => "App menu entries are up to date".to_string(),
                1 => "Created 1 app menu entry".to_string(),
                n => format!("Created {n} app menu entries"),
            },
        };
        self.reload();
        self.toast(&text);
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name(APP_NAME)
            .application_icon("system-users")
            .version(env!("CARGO_PKG_VERSION"))
            .comments("Run several Signal accounts side by side, each with its own messages and app menu entry.")
            .developer_name("VesperX")
            .website(env!("CARGO_PKG_HOMEPAGE"))
            .issue_url(concat!(env!("CARGO_PKG_REPOSITORY"), "/issues"))
            .copyright("© 2026 VesperX ApS")
            .build();
        about.add_link("Source Code", env!("CARGO_PKG_REPOSITORY"));
        about.present(Some(&self.window));
    }

    /// Installs Signal on a worker thread, then shows the result.
    pub async fn install(self: Rc<Self>) {
        self.install_page.show_installing();
        let installer = self.deps.installer.clone();
        let outcome = gio::spawn_blocking(move || installer.install())
            .await
            .unwrap_or_else(|_| {
                InstallOutcome::Failed("The installer stopped unexpectedly.".into())
            });
        match outcome {
            InstallOutcome::Installed => {
                self.install_page.show_idle();
                self.reload();
            }
            InstallOutcome::Cancelled => {
                self.install_page.show_idle();
                self.toast("Installation cancelled");
            }
            InstallOutcome::Failed(message) => self.install_page.show_failed(&message),
        }
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn toast_with_button(
        self: &Rc<Self>,
        text: &str,
        label: &str,
        on_click: impl Fn(&Rc<Self>) + 'static,
    ) {
        let toast = adw::Toast::builder()
            .title(text)
            .button_label(label)
            .build();
        let weak = Rc::downgrade(self);
        toast.connect_button_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                on_click(&this);
            }
        });
        *self.last_toast_button.borrow_mut() = Some(label.to_string());
        self.toasts.add_toast(toast);
    }

    // Signals and live status ----------------------------------------------

    fn connect_signals(self: &Rc<Self>) {
        let list = &self.sidebar.list;
        let weak = Rc::downgrade(self);
        list.connect_row_selected(move |_, row| {
            let (Some(this), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            if !this.rebuilding.get() {
                this.show_profile(&row.widget_name());
            }
        });

        // Split: double-click or Enter opens Signal. Folded: a click opens the
        // detail page.
        let weak = Rc::downgrade(self);
        list.connect_row_activated(move |_, row| {
            let Some(this) = weak.upgrade() else { return };
            if this.split.is_collapsed() {
                this.show_profile(&row.widget_name());
                this.split.set_show_content(true);
            } else {
                this.open_selected();
            }
        });

        let right_click = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        let weak = Rc::downgrade(self);
        right_click.connect_pressed(move |gesture, _, x, y| {
            if let Some(this) = weak.upgrade() {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                this.show_context_menu(x, y);
            }
        });
        list.add_controller(right_click);
        let long_press = gtk::GestureLongPress::builder().touch_only(true).build();
        let weak = Rc::downgrade(self);
        long_press.connect_pressed(move |_, x, y| {
            if let Some(this) = weak.upgrade() {
                this.show_context_menu(x, y);
            }
        });
        list.add_controller(long_press);

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            weak.upgrade()
                .map_or(glib::Propagation::Proceed, |this| this.handle_list_key(key))
        });
        list.add_controller(keys);

        let weak = Rc::downgrade(self);
        self.split.connect_collapsed_notify(move |_| {
            if let Some(this) = weak.upgrade()
                && this.stack.visible_child_name().as_deref() == Some("app")
            {
                this.render();
            }
        });

        let weak = Rc::downgrade(self);
        self.content.back.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.split.set_show_content(false);
            }
        });

        let bar = self.content.bar.clone();
        self.content
            .scroller
            .vadjustment()
            .connect_value_changed(move |adj| {
                set_class(&bar, "scrolled", adj.value() > DETAIL_SCROLLED_AT)
            });
        let weak = Rc::downgrade(self);
        self.sidebar
            .scroller
            .vadjustment()
            .connect_value_changed(move |adj| {
                if let Some(this) = weak.upgrade() {
                    let scrolled = this.split.is_collapsed() && adj.value() > LIST_SCROLLED_AT;
                    set_class(&this.sidebar.bar, "scrolled", scrolled);
                }
            });

        let weak = Rc::downgrade(self);
        self.install_page.button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                glib::spawn_future_local(this.install());
            }
        });

        // Sizes may have changed while the window was in the background.
        let weak = Rc::downgrade(self);
        self.window.connect_is_active_notify(move |window| {
            if let (true, Some(this)) = (window.is_active(), weak.upgrade()) {
                this.reload();
            }
        });

        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(2, move || match weak.upgrade() {
            Some(this) => {
                this.refresh_running();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });

        // The window owns the only strong reference, so MainWindow lives
        // exactly as long as the window.
        let this = self.clone();
        self.window.connect_close_request(move |_| {
            this.save_window_state();
            glib::Propagation::Proceed
        });
    }

    /// Reloads only when a profile started or stopped, so the 2-second poll
    /// never disturbs the selection or scroll position needlessly.
    fn refresh_running(&self) {
        if self.stack.visible_child_name().as_deref() != Some("app") {
            return;
        }
        let running = procs::running_data_dirs(&self.deps.store.paths.proc_root);
        let changed = self
            .profiles
            .borrow()
            .iter()
            .any(|p| p.running != running.contains(&p.dir));
        if changed {
            self.reload();
        }
    }

    fn handle_list_key(&self, key: gdk::Key) -> glib::Propagation {
        if matches!(key, gdk::Key::Delete | gdk::Key::KP_Delete) {
            // Fails only while the action is disabled (a running profile).
            let _ = WidgetExt::activate_action(&self.window, "win.trash-selected", None);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    }

    fn show_context_menu(&self, x: f64, y: f64) {
        let Some(row) = self.sidebar.list.row_at_y(y as i32) else {
            return;
        };
        let name = row.widget_name();
        if self.split.is_collapsed() {
            self.show_profile(&name);
        } else {
            self.sidebar.list.select_row(Some(&row));
        }
        self.context_menu
            .set_menu_model(Some(&self.context_model()));
        let at = self
            .sidebar
            .list
            .compute_point(
                &self.sidebar.body,
                &gtk::graphene::Point::new(x as f32, y as f32),
            )
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        self.context_menu.set_pointing_to(Some(&gdk::Rectangle::new(
            at.x() as i32,
            at.y() as i32,
            1,
            1,
        )));
        self.context_menu.popup();
    }

    fn context_model(&self) -> gio::Menu {
        let profile = self.selected_profile();
        let running = profile.as_ref().is_some_and(|p| p.running);
        let open = gio::Menu::new();
        open.append(
            Some(if running {
                "Show Signal"
            } else {
                "Open Signal"
            }),
            Some("win.open-selected"),
        );
        open.append(Some("Show in Files"), Some("win.show-selected"));
        let menu = gio::Menu::new();
        menu.append_section(None, &open);
        if !profile.is_some_and(|p| p.is_default) {
            let trash = gio::Menu::new();
            trash.append(Some("Move to Trash…"), Some("win.trash-selected"));
            menu.append_section(None, &trash);
        }
        menu
    }

    // Window size memory -------------------------------------------------------

    fn restore_window_state(&self) {
        let file = settings::load(&self.deps.store.paths.settings);
        if let (Ok(w), Ok(h)) = (
            file.integer("window", "width"),
            file.integer("window", "height"),
        ) && w >= 360
            && h >= 480
        {
            self.window.set_default_size(w, h);
        }
        if file.boolean("window", "maximized").unwrap_or(false) {
            self.window.maximize();
        }
    }

    /// Updates the window keys and keeps everything else in the file.
    fn save_window_state(&self) {
        let path = &self.deps.store.paths.settings;
        let file = settings::load(path);
        let (w, h) = self.window.default_size();
        file.set_integer("window", "width", w);
        file.set_integer("window", "height", h);
        file.set_boolean("window", "maximized", self.window.is_maximized());
        if let Err(e) = settings::save(path, &file) {
            eprintln!(
                "multisignal: could not save the window size to {}: {e}",
                path.display()
            );
        }
    }

    /// Applies and remembers Automatic, Light or Dark.
    fn set_appearance(&self, appearance: Appearance) {
        appearance.apply();
        if let Some(action) = self.window.lookup_action("appearance") {
            action.change_state(&appearance.id().to_variant());
        }
        let path = &self.deps.store.paths.settings;
        if let Err(e) = appearance.save(path) {
            self.toast(&format!("Could not save the appearance: {e}"));
        }
    }

    // Test accessors -----------------------------------------------------------

    pub fn visible_page(&self) -> String {
        self.stack
            .visible_child_name()
            .map(String::from)
            .unwrap_or_default()
    }

    pub fn sidebar_names(&self) -> Vec<String> {
        rows(&self.sidebar.list)
            .map(|r| r.widget_name().to_string())
            .collect()
    }

    pub fn selected(&self) -> Option<String> {
        self.selected.borrow().clone()
    }

    pub fn footer(&self) -> String {
        self.sidebar.footer.text().to_string()
    }

    /// The names shown in the sidebar, top to bottom.
    pub fn sidebar_titles(&self) -> Vec<String> {
        rows(&self.sidebar.list)
            .map(|r| sidebar_row::title_text(&r))
            .collect()
    }

    /// Checks a new profile name: the folder rules, plus the default
    /// Signal's own name so two rows can't look the same.
    pub(super) fn validate_new_name(&self, input: &str) -> create_dialog::Validation {
        match create_dialog::validate(&self.deps.store, input) {
            create_dialog::Validation::Ok => match self.default_title.borrow().as_ref() {
                Some(title) if title.eq_ignore_ascii_case(input.trim()) => {
                    create_dialog::Validation::Exists(title.clone())
                }
                _ => create_dialog::Validation::Ok,
            },
            other => other,
        }
    }

    pub fn sidebar_secondary(&self, name: &str) -> String {
        self.row(name)
            .map(|r| sidebar_row::secondary_text(&r))
            .unwrap_or_default()
    }

    pub fn detail_title(&self) -> String {
        self.detail
            .borrow()
            .as_ref()
            .map(|d| d.title.text().to_string())
            .unwrap_or_default()
    }

    pub fn detail_primary_label(&self) -> String {
        self.detail
            .borrow()
            .as_ref()
            .map(|d| button_label(&d.primary))
            .unwrap_or_default()
    }

    pub fn detail_value(&self, title: &str) -> String {
        self.detail
            .borrow()
            .as_ref()
            .and_then(|d| d.value(title))
            .unwrap_or_default()
    }

    pub fn detail_has_repair(&self) -> bool {
        self.detail
            .borrow()
            .as_ref()
            .is_some_and(|d| d.repair.is_some())
    }

    /// `name` without the "win." prefix is looked up on the window.
    pub fn action_enabled(&self, name: &str) -> bool {
        let name = name.strip_prefix("win.").unwrap_or(name);
        self.window
            .lookup_action(name)
            .is_some_and(|a| a.is_enabled())
    }

    pub fn context_menu_labels(&self) -> Vec<String> {
        menu_labels(self.context_model().upcast_ref())
    }

    pub fn activate_action_for_test(&self, name: &str) {
        gio::prelude::ActionGroupExt::activate_action(&self.window, name, None);
    }

    /// The chosen appearance: "system", "light" or "dark".
    pub fn appearance(&self) -> String {
        self.window
            .lookup_action("appearance")
            .and_then(|a| a.state())
            .and_then(|s| s.get::<String>())
            .unwrap_or_default()
    }

    pub fn more_menu_labels(&self) -> Vec<String> {
        self.content
            .more
            .menu_model()
            .map(|m| menu_labels(&m))
            .unwrap_or_default()
    }

    /// Opens the "⋯" menu of the detail bar and returns its popover.
    pub fn open_more_menu_for_test(&self) -> Option<gtk::Popover> {
        self.content.more.popup();
        self.content.more.popover()
    }

    pub fn set_appearance_for_test(&self, id: &str) {
        gio::prelude::ActionGroupExt::activate_action(
            &self.window,
            "appearance",
            Some(&id.to_variant()),
        );
    }

    pub fn save_window_state_for_test(&self) {
        self.save_window_state();
    }

    pub fn last_toast_button_for_test(&self) -> Option<String> {
        self.last_toast_button.borrow().clone()
    }

    /// Reads everything again, as the 2-second poll or regaining focus does.
    pub fn refresh_for_test(&self) {
        self.reload();
    }

    pub fn set_collapsed_for_test(&self, collapsed: bool) {
        self.split.set_collapsed(collapsed);
    }

    pub fn showing_content(&self) -> bool {
        self.split.shows_content()
    }

    pub fn go_back_for_test(&self) {
        self.content.back.emit_clicked();
    }

    pub fn press_delete_for_test(&self) -> bool {
        self.handle_list_key(gdk::Key::Delete) == glib::Propagation::Stop
    }

    pub fn run_install_for_test(self: &Rc<Self>) {
        glib::MainContext::default().block_on(self.clone().install());
    }
}

fn build_sidebar() -> Sidebar {
    let title = gtk::Label::builder()
        .label(APP_NAME)
        .css_classes(["sidebar-title"])
        .build();
    let more = more_menu_button();
    let bar = adw::HeaderBar::builder().title_widget(&title).build();
    bar.pack_end(&new_profile_button());
    bar.pack_end(&more);

    let large_summary = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["large-summary", "numeric"])
        .build();
    let large_title = gtk::Box::new(gtk::Orientation::Vertical, 2);
    large_title.add_css_class("large-title-box");
    large_title.append(
        &gtk::Label::builder()
            .label(APP_NAME)
            .xalign(0.0)
            .css_classes(["large-title"])
            .build(),
    );
    large_title.append(&large_summary);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.update_property(&[gtk::accessible::Property::Label("Profiles")]);
    let note = gtk::Label::builder()
        .label(
            "Each profile is a separate Signal account with its own messages and app menu entry.",
        )
        .xalign(0.0)
        .wrap(true)
        .css_classes(["compact-note"])
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&large_title);
    body.append(&list);
    body.append(&note);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&body)
        .build();

    let footer = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["sidebar-footer", "numeric"])
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&bar);
    toolbar.set_content(Some(&scroller));
    toolbar.add_bottom_bar(&footer);

    Sidebar {
        toolbar,
        body,
        bar,
        title,
        more,
        scroller,
        large_title,
        large_summary,
        list,
        note,
        footer,
    }
}

fn build_content() -> Content {
    let title = gtk::Label::builder()
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["bar-title"])
        .build();
    let back_content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    back_content.append(&gtk::Image::from_icon_name("go-previous-symbolic"));
    back_content.append(&gtk::Label::new(Some("Profiles")));
    let back = gtk::Button::builder()
        .child(&back_content)
        .css_classes(["flat", "back-button"])
        .tooltip_text("Back to Profiles")
        .visible(false)
        .build();
    let more = more_menu_button();
    let bar = adw::HeaderBar::builder()
        .title_widget(&title)
        .show_back_button(false)
        .build();
    bar.pack_start(&back);
    bar.pack_end(&more);

    let clamp = adw::Clamp::builder().maximum_size(680).build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&bar);
    toolbar.set_content(Some(&scroller));
    let page = adw::NavigationPage::builder()
        .title("Profile")
        .child(&toolbar)
        .build();

    Content {
        page,
        bar,
        title,
        back,
        more,
        scroller,
        clamp,
    }
}

/// A full-window message (empty, not installed): 80 px icon, large title,
/// one line of text, and a box for the action. Returns (page, bar, actions).
pub(super) fn whole_window_page(
    icon: &str,
    title: &str,
    text: &str,
) -> (adw::ToolbarView, adw::HeaderBar, gtk::Box) {
    let image = gtk::Image::builder()
        .icon_name(icon)
        .pixel_size(80)
        .css_classes(["page-icon"])
        .build();
    let title = gtk::Label::builder()
        .label(title)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["page-title"])
        .build();
    let text = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .max_width_chars(46)
        .css_classes(["page-body"])
        .build();
    let actions = gtk::Box::new(gtk::Orientation::Vertical, 12);
    actions.set_halign(gtk::Align::Center);
    actions.set_margin_top(14);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_valign(gtk::Align::Center);
    body.set_halign(gtk::Align::Center);
    body.set_margin_start(32);
    body.set_margin_end(32);
    body.set_margin_bottom(32);
    body.append(&image);
    body.append(&title);
    body.append(&text);
    body.append(&actions);

    let bar = adw::HeaderBar::builder()
        .title_widget(&gtk::Box::new(gtk::Orientation::Horizontal, 0))
        .build();
    let page = adw::ToolbarView::new();
    page.add_top_bar(&bar);
    page.set_content(Some(&body));
    (page, bar, actions)
}

fn new_profile_button() -> gtk::Button {
    gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .action_name("win.new-profile")
        .tooltip_text("New Profile")
        .css_classes(["flat", "accent-icon"])
        .build()
}

fn more_menu_button() -> gtk::MenuButton {
    let repair = gio::Menu::new();
    repair.append(Some("Repair App Menu Entries"), Some("win.repair-all"));
    let appearance = gio::Menu::new();
    for choice in Appearance::ALL {
        appearance.append(
            Some(choice.label()),
            Some(&format!("win.appearance::{}", choice.id())),
        );
    }
    let links = gio::Menu::new();
    links.append(Some("Handle Signal Links"), Some("win.handle-links"));
    let about = gio::Menu::new();
    about.append(Some("About Signal Profiles"), Some("win.about"));
    let menu = gio::Menu::new();
    menu.append_section(None, &repair);
    menu.append_section(Some("Appearance"), &appearance);
    menu.append_section(None, &links);
    menu.append_section(None, &about);
    let button = gtk::MenuButton::builder()
        .icon_name("view-more-horizontal-symbolic")
        .menu_model(&menu)
        .tooltip_text("More")
        .css_classes(["flat", "accent-icon"])
        .build();
    // Desktop menus drop down without a pointer arrow.
    if let Some(popover) = button.popover() {
        popover.set_has_arrow(false);
    }
    button
}

fn rows(list: &gtk::ListBox) -> impl Iterator<Item = gtk::ListBoxRow> + '_ {
    (0..).map_while(|i| list.row_at_index(i))
}

fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// Item labels of a menu model, sections flattened.
fn menu_labels(model: &gio::MenuModel) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..model.n_items() {
        if let Some(label) = model
            .item_attribute_value(i, "label", Some(glib::VariantTy::STRING))
            .and_then(|v| v.get::<String>())
        {
            out.push(label);
        }
        if let Some(section) = model.item_link(i, "section") {
            out.extend(menu_labels(&section));
        }
    }
    out
}
