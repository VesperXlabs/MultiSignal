//! Builds the real window against fixture profiles and fake side effects,
//! then inspects the widget tree. GTK must run on the main thread, so this is
//! one binary with its own `main` (harness = false). Needs a display; the
//! desktop session provides one.

use adw::prelude::*;
use multisignal::paths::Paths;
use multisignal::store::{Launch, Store, Trash};
use multisignal::system::{InstallOutcome, Installer};
use multisignal::ui::{self, Deps};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Moves into a directory next to the fixture, like the real Trash would.
struct DirTrash(PathBuf);
impl Trash for DirTrash {
    fn trash(&self, path: &Path) -> Result<(), String> {
        std::fs::create_dir_all(&self.0).unwrap();
        std::fs::rename(path, self.0.join(path.file_name().unwrap())).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Default)]
struct Recorder(Rc<RefCell<Vec<String>>>);
impl Launch for Recorder {
    /// Records "name", or "name link" when a link is passed.
    fn launch(&self, _: &Paths, name: &str, link: Option<&str>) -> std::io::Result<()> {
        self.0.borrow_mut().push(record(name, link));
        Ok(())
    }
    fn launch_default(&self, _: &Paths, link: Option<&str>) -> std::io::Result<()> {
        self.0.borrow_mut().push(record("(default)", link));
        Ok(())
    }
}

fn record(name: &str, link: Option<&str>) -> String {
    link.map_or_else(|| name.to_string(), |l| format!("{name} {l}"))
}

/// "Installs" by flipping a flag, as a successful snap install would.
struct FakeInstaller(Arc<AtomicBool>);
impl Installer for FakeInstaller {
    fn is_installed(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn install(&self) -> InstallOutcome {
        self.0.store(true, Ordering::SeqCst);
        InstallOutcome::Installed
    }
    fn other_install(&self) -> Option<&'static str> {
        None
    }
}

struct Fixture {
    deps: Deps,
    paths: Paths,
    launched: Recorder,
}

fn fixture(home: &Path, installed: bool, profiles: &[&str], running: &[&str]) -> Fixture {
    let mut paths = Paths::for_home(home, None);
    paths.proc_root = home.join("proc");
    std::fs::create_dir_all(&paths.proc_root).unwrap();
    for p in profiles {
        std::fs::create_dir_all(paths.profile_dir(p)).unwrap();
    }
    for (i, r) in running.iter().enumerate() {
        let pid = paths.proc_root.join((100 + i).to_string());
        std::fs::create_dir_all(&pid).unwrap();
        std::fs::write(
            pid.join("cmdline"),
            format!(
                "/x/signal-desktop\0--user-data-dir={}\0",
                paths.profile_dir(r).display()
            ),
        )
        .unwrap();
    }
    let launched = Recorder::default();
    let deps = Deps {
        store: Store::new(
            paths.clone(),
            Box::new(DirTrash(home.join("trash"))),
            Box::new(launched.clone()),
        ),
        installer: Arc::new(FakeInstaller(Arc::new(AtomicBool::new(installed)))),
    };
    Fixture {
        deps,
        paths,
        launched,
    }
}

fn deps(home: &Path, installed: bool, profiles: &[&str], running: &[&str]) -> Deps {
    fixture(home, installed, profiles, running).deps
}

fn check(name: &str, ok: bool) {
    println!("{} {name}", if ok { "ok  " } else { "FAIL" });
    if !ok {
        std::process::exit(1);
    }
}

/// Points GIO at sandbox folders before it starts, so link-handler tests
/// never touch the real mimeapps.list; our desktop entry is installed there.
fn sandbox_gio(root: &Path) {
    let data = root.join("data");
    let apps = data.join("applications");
    std::fs::create_dir_all(&apps).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/io.github.multisignal.MultiSignal.desktop"
        ),
        apps.join("io.github.multisignal.MultiSignal.desktop"),
    )
    .unwrap();
    // GIO ignores entries whose program isn't on the PATH, and `multisignal`
    // is only there where the app is installed; a stub stands in for it.
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("multisignal"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        bin.join("multisignal"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // SAFETY: first thing in main, before GTK or any other thread starts.
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("HOME", root);
        std::env::set_var("XDG_DATA_HOME", &data);
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_DIRS", "/usr/local/share:/usr/share");
    }
}

fn main() {
    let tmp = tempfile::tempdir().unwrap();
    sandbox_gio(&tmp.path().join("gio"));
    adw::init().expect("a display is available");

    // Task 10: window shell, states, split view.
    let w = ui::build_window(deps(&tmp.path().join("a"), false, &[], &[]));
    check(
        "not installed → install page",
        w.visible_page() == "install",
    );

    let w = ui::build_window(deps(&tmp.path().join("b"), true, &[], &[]));
    check("no profiles → empty page", w.visible_page() == "empty");

    let w = ui::build_window(deps(
        &tmp.path().join("c"),
        true,
        &["UKR", "Damon", "Personal"],
        &["UKR"],
    ));
    check("profiles → app page", w.visible_page() == "app");
    check(
        "sidebar sorted by name",
        w.sidebar_names() == ["Damon", "Personal", "UKR"],
    );
    check(
        "first profile selected on start",
        w.selected().as_deref() == Some("Damon"),
    );
    check(
        "footer summary",
        w.footer() == "3 profiles · 1 running · Empty",
    );
    check(
        "default size is desktop-sized",
        w.window.default_width() == 1100 && w.window.default_height() == 720,
    );

    w.set_collapsed_for_test(true);
    w.select("UKR");
    check(
        "collapsed: selecting shows the detail page",
        w.showing_content(),
    );
    w.go_back_for_test();
    check("collapsed: back returns to the list", !w.showing_content());
    w.set_collapsed_for_test(false);

    // The stylesheets parse: GTK CSS is not web CSS and only warns at runtime.
    let errors = ui::css_errors();
    check(&format!("CSS parses cleanly {errors:?}"), errors.is_empty());

    // Task 11: sidebar rows, detail pane, actions.
    let home = tmp.path().join("e");
    let f = fixture(&home, true, &["UKR", "Damon", "Personal"], &["UKR"]);
    let paths = f.paths.clone();
    let launched = f.launched.clone();
    multisignal::launcher::write(&paths, "UKR").unwrap();
    std::fs::write(
        paths.applications.join("signal-desktop-damon.desktop"),
        format!(
            "[Desktop Entry]\nName=Damon\nExec=env X=1 /snap/bin/signal-desktop --user-data-dir={} %U\n",
            paths.profile_dir("Damon").display()
        ),
    )
    .unwrap();
    let w = ui::build_window(f.deps);
    check(
        "running row says Running",
        w.sidebar_secondary("UKR").starts_with("Running"),
    );
    check(
        "missing launcher row warns",
        w.sidebar_secondary("Personal")
            .starts_with("No app menu entry"),
    );
    check(
        "stopped row shows only the size",
        w.sidebar_secondary("Damon") == "Empty",
    );
    w.select("UKR");
    check("detail title", w.detail_title() == "UKR");
    check(
        "running: primary button says Show Signal",
        w.detail_primary_label() == "Show Signal",
    );
    check(
        "running: trash disabled",
        !w.action_enabled("win.trash-selected"),
    );
    check(
        "running: context menu says Show Signal",
        w.context_menu_labels()[0] == "Show Signal",
    );
    w.select("Damon");
    check(
        "stopped: primary says Open Signal",
        w.detail_primary_label() == "Open Signal",
    );
    check(
        "stopped: trash enabled",
        w.action_enabled("win.trash-selected"),
    );
    check(
        "hand-made launcher is labelled",
        w.detail_value("Launcher file") == "signal-desktop-damon.desktop · made by hand",
    );
    check(
        "menu entry shows the launcher's own name",
        w.detail_value("Menu entry") == "Damon",
    );
    check(
        "context menu has open, show, trash",
        w.context_menu_labels() == ["Open Signal", "Show in Files", "Move to Trash…"],
    );
    w.activate_action_for_test("open-selected");
    check(
        "Open Signal launches the selected profile",
        *launched.0.borrow() == ["Damon"],
    );
    w.select("Personal");
    check("missing launcher offers Repair", w.detail_has_repair());
    w.activate_action_for_test("repair");
    check(
        "Repair writes the launcher",
        paths.own_launcher("Personal").is_file(),
    );
    check(
        "after Repair the row no longer warns",
        w.sidebar_secondary("Personal") == "Empty",
    );
    check("after Repair no Repair button", !w.detail_has_repair());
    check(
        "selection survives a reload",
        w.selected().as_deref() == Some("Personal"),
    );

    // Task 12: dialogs.
    use multisignal::ui::create_dialog::{Validation, validate};
    let d = deps(&tmp.path().join("f"), true, &["Work"], &[]);
    check("valid name", validate(&d.store, "Travel") == Validation::Ok);
    check(
        "space → suggestion",
        validate(&d.store, "My Work")
            == Validation::Invalid {
                suggestion: Some("My-Work".into()),
            },
    );
    check(
        "case duplicate",
        validate(&d.store, "work") == Validation::Exists("Work".into()),
    );
    check(
        "empty shows the hint, not an error",
        validate(&d.store, "") == Validation::Empty,
    );
    check(
        "blank shows the hint, not an error",
        validate(&d.store, "   ") == Validation::Empty,
    );

    let f = fixture(
        &tmp.path().join("g"),
        true,
        &["Damon", "Personal", "Work"],
        &[],
    );
    let paths = f.paths.clone();
    let w = ui::build_window(f.deps);
    let dialog = w.open_create_dialog();
    dialog.entry.set_text("My Work");
    check(
        "invalid name disables Create",
        !dialog.create.is_sensitive(),
    );
    check(
        "invalid name offers the fix",
        dialog.suggestion.get_visible(),
    );
    dialog.suggestion.emit_clicked();
    check("fix-it fills the field", dialog.entry.text() == "My-Work");
    check("fixed name enables Create", dialog.create.is_sensitive());
    dialog.entry.set_text("Travel");
    dialog.create.emit_clicked();
    check(
        "create writes the profile",
        paths.own_launcher("Travel").is_file(),
    );
    check(
        "new profile selected",
        w.selected().as_deref() == Some("Travel"),
    );
    check(
        "new profile listed",
        w.sidebar_names() == ["Damon", "Personal", "Travel", "Work"],
    );

    w.select("Personal");
    let trash = w
        .open_trash_dialog()
        .expect("a stopped profile is selected");
    trash.cancel.emit_clicked();
    check(
        "cancel keeps the profile",
        paths.profile_dir("Personal").is_dir(),
    );
    let trash = w
        .open_trash_dialog()
        .expect("a stopped profile is selected");
    check(
        "trash dialog names the profile",
        trash.title.text() == "Move “Personal” to the Trash?",
    );
    trash.confirm.emit_clicked();
    check(
        "confirm trashes the data",
        !paths.profile_dir("Personal").exists(),
    );
    check(
        "confirm trashes into the Trash",
        tmp.path().join("g/trash/Personal").is_dir(),
    );
    check(
        "neighbour selected after delete",
        w.selected().as_deref() == Some("Travel"),
    );
    check(
        "deleted profile unlisted",
        w.sidebar_names() == ["Damon", "Travel", "Work"],
    );
    check(
        "Delete key opens the trash dialog",
        w.press_delete_for_test(),
    );

    // The snap's own (default) Signal: listed first, never trashed.
    use multisignal::profiles::DEFAULT_NAME;
    let f = fixture(&tmp.path().join("i"), true, &["Work"], &[]);
    let launched = f.launched.clone();
    std::fs::create_dir_all(&f.paths.default_data_dir).unwrap();
    let pid = f.paths.proc_root.join("300");
    std::fs::create_dir_all(&pid).unwrap();
    std::fs::write(
        pid.join("cmdline"),
        "/snap/signal-desktop/945/opt/Signal/signal-desktop --no-sandbox --disable-gpu\0",
    )
    .unwrap();
    let w = ui::build_window(f.deps);
    check(
        "default Signal listed first",
        w.sidebar_names() == [DEFAULT_NAME, "Work"],
    );
    check(
        "default Signal selected on start",
        w.selected().as_deref() == Some(DEFAULT_NAME),
    );
    check(
        "default Signal shows Running",
        w.sidebar_secondary(DEFAULT_NAME).starts_with("Running"),
    );
    check(
        "actions are enabled on start, before any click",
        w.action_enabled("win.open-selected"),
    );
    check(
        "default Signal: Show Signal",
        w.detail_primary_label() == "Show Signal",
    );
    check(
        "default Signal: menu entry is Signal",
        w.detail_value("Menu entry") == "Signal",
    );
    check(
        "default Signal: no trash",
        !w.action_enabled("win.trash-selected"),
    );
    check("default Signal: no repair", !w.action_enabled("win.repair"));
    check(
        "default Signal: context menu has no trash",
        w.context_menu_labels() == ["Show Signal", "Show in Files"],
    );
    w.activate_action_for_test("open-selected");
    check(
        "default Signal: open starts the default",
        *launched.0.borrow() == ["(default)"],
    );
    w.select("Work");
    check(
        "a profile after it can still be trashed",
        w.action_enabled("win.trash-selected"),
    );

    // The default Signal can carry the user's own name for it.
    let f = fixture(&tmp.path().join("k"), true, &["Work"], &[]);
    let launched = f.launched.clone();
    std::fs::create_dir_all(&f.paths.default_data_dir).unwrap();
    std::fs::create_dir_all(f.paths.settings.parent().unwrap()).unwrap();
    std::fs::write(&f.paths.settings, "[default]\nname=Personal\n").unwrap();
    let w = ui::build_window(f.deps);
    check(
        "default shown by its own name, still marked (default)",
        w.sidebar_titles() == ["Personal (default)", "Work"],
    );
    check(
        "its detail uses the name",
        w.detail_title() == "Personal (default)",
    );
    w.activate_action_for_test("open-selected");
    check(
        "renamed default still opens the default",
        *launched.0.borrow() == ["(default)"],
    );
    let dialog = w.open_create_dialog();
    dialog.entry.set_text("personal");
    check(
        "its name can't be reused for a profile",
        !dialog.create.is_sensitive(),
    );
    dialog.dialog.close();
    let w = ui::build_window(deps(&tmp.path().join("l"), true, &["Work"], &[]));
    check(
        "profiles are shown by their folder name",
        w.sidebar_titles() == ["Work"],
    );

    // Moving the default Signal into a profile ("Empty" it without losing it).
    let f = fixture(&tmp.path().join("m"), true, &["Work"], &[]);
    let paths = f.paths.clone();
    std::fs::create_dir_all(&paths.default_data_dir).unwrap();
    std::fs::write(paths.default_data_dir.join("db"), "messages").unwrap();
    std::fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
    std::fs::write(&paths.settings, "[default]\nname=Personal\n").unwrap();
    let pid = paths.proc_root.join("400");
    std::fs::create_dir_all(&pid).unwrap();
    std::fs::write(pid.join("cmdline"), "/snap/bin/signal-desktop\0").unwrap();
    let w = ui::build_window(f.deps);
    check(
        "running default: can't be moved",
        !w.action_enabled("win.adopt-default"),
    );
    std::fs::remove_dir_all(&pid).unwrap();
    w.refresh_for_test();
    check(
        "stopped default: can be moved",
        w.action_enabled("win.adopt-default"),
    );
    let dialog = w.open_adopt_dialog();
    check(
        "move dialog suggests the default's name",
        dialog.entry.text() == "Personal",
    );
    check("that name is accepted", dialog.create.is_sensitive());
    dialog.create.emit_clicked();
    check(
        "the data moved into the profile",
        std::fs::read_to_string(paths.profile_dir("Personal").join("db")).is_ok(),
    );
    check(
        "the default stays listed, empty",
        w.sidebar_titles() == ["Signal (default)", "Personal", "Work"]
            && w.sidebar_secondary(DEFAULT_NAME) == "Empty",
    );
    check(
        "the moved profile is selected",
        w.selected().as_deref() == Some("Personal"),
    );
    check(
        "its old display name is cleared",
        !std::fs::read_to_string(&paths.settings)
            .unwrap_or_default()
            .contains("name=Personal"),
    );
    check(
        "after moving, the toast offers to lock the default",
        w.last_toast_button_for_test().as_deref() == Some("Lock"),
    );
    w.select(DEFAULT_NAME);
    check(
        "the empty default can be locked",
        w.action_enabled("win.lock-default"),
    );
    check(
        "…but has nothing to move",
        !w.action_enabled("win.adopt-default"),
    );
    check("Work is not movable", {
        w.select("Work");
        !w.action_enabled("win.adopt-default")
    });

    // Locking the default Signal hides its menu entry and blocks opening it.
    let f = fixture(&tmp.path().join("n"), true, &["Work"], &[]);
    let paths = f.paths.clone();
    std::fs::create_dir_all(&paths.default_data_dir).unwrap();
    let w = ui::build_window(f.deps);
    w.select(DEFAULT_NAME);
    check(
        "unlocked default can be opened",
        w.action_enabled("win.open-selected"),
    );
    w.activate_action_for_test("lock-default");
    check(
        "Lock hides the snap's entry",
        multisignal::lock::is_locked(&paths),
    );
    check(
        "locked default says so",
        w.sidebar_secondary(DEFAULT_NAME).starts_with("Locked"),
    );
    check(
        "locked default can't be opened",
        !w.action_enabled("win.open-selected"),
    );
    check(
        "locked default: Unlock offered",
        !w.action_enabled("win.lock-default"),
    );
    w.activate_action_for_test("unlock-default");
    check(
        "Unlock shows it again",
        !multisignal::lock::is_locked(&paths),
    );
    check(
        "unlocked again: can be opened",
        w.action_enabled("win.open-selected"),
    );

    // Signal links: the one running profile gets them, otherwise the user picks.
    let f = fixture(&tmp.path().join("o"), true, &["A", "B", "C"], &["B"]);
    let launched = f.launched.clone();
    let w = ui::build_window(f.deps);
    check(
        "one running profile: no question",
        w.open_link("signalcaptcha://one").is_none(),
    );
    check(
        "the running profile got the link",
        *launched.0.borrow() == ["B signalcaptcha://one"],
    );
    check(
        "other links are ignored",
        w.open_link("https://example.com").is_none() && launched.0.borrow().len() == 1,
    );

    let f = fixture(&tmp.path().join("p"), true, &["A", "B", "C"], &["A", "C"]);
    let launched = f.launched.clone();
    let w = ui::build_window(f.deps);
    let chooser = w.open_link("sgnl://two").expect("several running: it asks");
    check(
        "it offers the running profiles",
        chooser.titles() == ["A", "C"],
    );
    chooser.choose("C");
    check(
        "the chosen profile got the link",
        *launched.0.borrow() == ["C sgnl://two"],
    );

    // The real application path: a link opened on the app reaches a profile.
    let f = fixture(&tmp.path().join("q"), true, &["Work"], &["Work"]);
    let launched = f.launched.clone();
    let paths = f.paths.clone();
    let recorder = f.launched.clone();
    let app = ui::application("io.github.multisignal.UiTest", move || Deps {
        store: Store::new(
            paths.clone(),
            Box::new(DirTrash(paths.signal_base.join("../trash"))),
            Box::new(recorder.clone()),
        ),
        installer: Arc::new(FakeInstaller(Arc::new(AtomicBool::new(true)))),
    });
    app.set_flags(app.flags() | gtk::gio::ApplicationFlags::NON_UNIQUE);
    let quit = app.clone();
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || quit.quit());
    app.run_with_args(&["multisignal", "signalcaptcha://three"]);
    check(
        "a link opened on the app reaches the running profile",
        *launched.0.borrow() == ["Work signalcaptcha://three"],
    );
    check("…and shows the window", app.active_window().is_some());

    // Being the handler for Signal links (in the GIO sandbox).
    use multisignal::ui::link_handler;
    check("not the link handler at first", !link_handler::is_default());
    let settings = tmp.path().join("gio/claim.ini");
    link_handler::claim_once(&settings);
    check(
        "first run makes it the link handler",
        link_handler::is_default(),
    );
    link_handler::set_default(false).unwrap();
    link_handler::claim_once(&settings);
    check(
        "…only once: a later choice is respected",
        !link_handler::is_default(),
    );
    check(
        "the ⋯ menu has Handle Signal Links",
        w.more_menu_labels()
            .iter()
            .any(|l| l == "Handle Signal Links"),
    );
    w.activate_action_for_test("handle-links");
    check(
        "Handle Signal Links turns it on",
        link_handler::is_default(),
    );
    w.activate_action_for_test("handle-links");
    check("…and off again", !link_handler::is_default());

    // Appearance: Automatic (follow GNOME), Light or Dark, remembered.
    let style = adw::StyleManager::default();
    style.set_color_scheme(adw::ColorScheme::PreferLight);
    let home = tmp.path().join("j");
    let f = fixture(&home, true, &["Work"], &[]);
    let settings = f.paths.settings.clone();
    let w = ui::build_window(f.deps);
    check(
        "no saved appearance leaves the theme alone",
        style.color_scheme() == adw::ColorScheme::PreferLight,
    );
    check(
        "the ⋯ menu offers Automatic, Light and Dark",
        ["Automatic", "Light", "Dark"]
            .iter()
            .all(|l| w.more_menu_labels().iter().any(|m| m == l)),
    );
    check("Automatic is chosen by default", w.appearance() == "system");
    w.set_appearance_for_test("dark");
    check(
        "Dark forces the dark style",
        style.color_scheme() == adw::ColorScheme::ForceDark,
    );
    check("Dark is chosen", w.appearance() == "dark");
    let saved = std::fs::read_to_string(&settings).unwrap_or_default();
    check("the choice is saved", saved.contains("mode=dark"));
    style.set_color_scheme(adw::ColorScheme::Default);
    let w = ui::build_window(fixture(&home, true, &["Work"], &[]).deps);
    check(
        "the saved choice is applied on start",
        style.color_scheme() == adw::ColorScheme::ForceDark && w.appearance() == "dark",
    );
    w.set_appearance_for_test("light");
    check(
        "Light forces the light style",
        style.color_scheme() == adw::ColorScheme::ForceLight,
    );
    w.set_appearance_for_test("system");
    check(
        "Automatic follows GNOME again",
        style.color_scheme() == adw::ColorScheme::Default,
    );
    w.save_window_state_for_test();
    let saved = std::fs::read_to_string(&settings).unwrap_or_default();
    check(
        "saving the window size keeps the appearance",
        saved.contains("mode=system") && saved.contains("width="),
    );

    // About: credits VesperX and links its site and the repository.
    let w = ui::build_window(deps(&tmp.path().join("k"), true, &["Work"], &[]));
    w.activate_action_for_test("about");
    let about = w.window.visible_dialog().and_downcast::<adw::AboutDialog>();
    check("About opens from the ⋯ menu", about.is_some());
    let about = about.unwrap();
    check("About names VesperX", about.developer_name() == "VesperX");
    check(
        "About links vesperx.dk",
        about.website() == "https://vesperx.dk",
    );
    check(
        "About carries the VesperX ApS copyright",
        about.copyright().contains("VesperX ApS"),
    );
    check(
        "About reports issues to the VesperXlabs repository",
        about.issue_url() == "https://github.com/VesperXlabs/MultiSignal/issues",
    );
    about.close();

    // Task 13: install page.
    let w = ui::build_window(deps(&tmp.path().join("h"), false, &[], &[]));
    check("install page shown", w.visible_page() == "install");
    w.run_install_for_test();
    check("after install → empty page", w.visible_page() == "empty");

    println!("ui: all checks passed");
}
