//! The Android shell: storage folders, `Services` backed by the Java activity, the inbox,
//! working copies, finger gestures, the on-screen keyboard and the eframe runner.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gridcraft_engine::Session;
use gridcraft_ui_egui::{Inbox, Services, SheetApp};
use serde_json::json;
use winit::platform::android::activity::AndroidApp;

use crate::mailbox::{self, Message, Outcome, Purpose};
use crate::places::Places;
use crate::workcopies::{Notice, State, WorkCopies};
use crate::{bridge, keys, touch::Touches};

/// The app's private folders (under `Context.getFilesDir()`, the same folders the Java side uses).
#[derive(Clone)]
struct Dirs {
    /// Settings (`ui.json`, `prefs.json`) and the working copies' index.
    profile: PathBuf,
    /// Messages from Java ([`mailbox`]).
    inbox: PathBuf,
    /// Files waiting for Java to save or print them.
    outbox: PathBuf,
    /// Working copies of workbooks opened from storage ([`crate::workcopies`]).
    docs: PathBuf,
    /// The system clipboard's text, as the activity last saw it.
    clipboard: PathBuf,
    /// Its table, when it holds one (`text/html`).
    clipboard_html: PathBuf,
}

impl Dirs {
    fn new(files: &Path) -> Self {
        Self {
            profile: files.join("profile"),
            inbox: files.join("inbox"),
            outbox: files.join("outbox"),
            docs: files.join("docs"),
            clipboard: files.join("clipboard.txt"),
            clipboard_html: files.join("clipboard.html"),
        }
    }
}

pub fn run(android: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("GridCraft"));
    std::panic::set_hook(Box::new(|info| log::error!("panic: {info}")));
    let files = android.internal_data_path().unwrap_or_else(|| PathBuf::from("/data/local/tmp/gridcraft"));
    let dirs = Dirs::new(&files);
    for d in [&dirs.profile, &dirs.inbox, &dirs.outbox, &dirs.docs] {
        if let Err(e) = std::fs::create_dir_all(d) {
            log::warn!("cannot create {}: {e}", d.display());
        }
    }
    // Files left in the outbox were handed to an earlier run's save picker and are done with.
    clear_dir(&dirs.outbox);
    // SAFETY: called on the app thread before eframe or any worker of ours starts, so nothing in
    // this process reads the environment concurrently. The user font folders go by HOME.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("HOME", &files);
    }
    log::info!("GridCraft {} for Android, data in {}", env!("CARGO_PKG_VERSION"), files.display());

    bridge::init(&android);
    let options = eframe::NativeOptions { android_app: Some(android), ..Default::default() };
    let result = eframe::run_native(
        "GridCraft",
        options,
        Box::new(move |cc| {
            if let Some(rs) = &cc.wgpu_render_state {
                log::info!("wgpu backend {:?} on {}", rs.adapter.get_info().backend, rs.adapter.get_info().name);
            }
            Ok(Box::new(Shell::new(&cc.egui_ctx, dirs)))
        }),
    );
    if let Err(e) = result {
        log::error!("GridCraft stopped: {e}");
    }
}

/// A file handed to the save picker, by ticket.
struct SaveJob {
    /// The name the app asked for.
    name: String,
    /// The workbook it is a copy of (the active one when the save started), once known.
    doc: Option<u64>,
}

#[derive(Default)]
struct Saves {
    next: u64,
    jobs: BTreeMap<u64, SaveJob>,
}

struct Shell {
    app: SheetApp,
    dirs: Dirs,
    /// Workbooks to open that have no file the app may write back to, by name and bytes.
    inbox: Inbox,
    /// Where a picked picture goes (Insert › Pictures hands it over when it asks for one).
    pictures: Rc<RefCell<Option<Inbox>>>,
    touches: Touches,
    /// Pickers and saves started and not yet answered (logged when the app closes with any).
    waiting: Rc<Cell<u32>>,
    /// The keyboard as last asked of the activity.
    keyboard: bool,
    /// Where the last tap or press began.
    press: Option<egui::Pos2>,
    /// When (seconds, egui's clock) and where the last tap ended: a text field that takes the
    /// keyboard's focus soon after, under that tap (double-tap on a cell, a tap on the formula
    /// bar or a box), shows the on-screen keyboard. A field the app focuses by itself (a filter
    /// menu's search box) doesn't.
    tapped_at: Option<(f64, egui::Pos2)>,
    /// Enter or Tab ended a cell's edit: the keyboard stays up for the next cell, as in Excel,
    /// until a tap elsewhere.
    entering: bool,
    /// Whether a cell was being edited in the last frame.
    cell_editing: bool,
    /// The ribbon's state before the keyboard came up for typing into cells (the ribbon folds
    /// away meanwhile, to leave the sheet room above the keyboard).
    ribbon_before_keyboard: Option<bool>,
    /// Where workbooks are, in the user's words (File › Info, Recent).
    places: Rc<RefCell<Places>>,
    /// Messages for the user from the host (a picker that couldn't open).
    messages: Rc<RefCell<Vec<String>>>,
    /// `ui.json` and `prefs.json` as last written.
    prefs_written: (Option<String>, Option<String>),
    /// The window edges covered by system bars or the keyboard (pixels).
    insets: Option<egui::epaint::MarginF32>,
    /// Whether the system bars' icons were last set for a light app (dark icons).
    light_bars: Option<bool>,
    copies: WorkCopies,
    saves: Rc<RefCell<Saves>>,
    /// The working copies open in the last frame, to notice one being closed.
    open_copies: Vec<String>,
    /// Whether egui-winit's own clipboard holds [`PASTE_SEED`] yet.
    seeded: bool,
}

/// What egui-winit's own clipboard holds so a hardware keyboard's Ctrl+V reaches the app.
/// egui-winit can't read Android's clipboard: it turns Ctrl+V into a paste of its own copy, and
/// into nothing at all while that copy is empty. Seeded with this invisible character, every
/// Ctrl+V arrives as a paste, whose text the shell then takes from the system clipboard.
const PASTE_SEED: &str = "\u{2063}";

impl Shell {
    fn new(ctx: &egui::Context, dirs: Dirs) -> Self {
        let inbox = Inbox::default();
        let pictures = Rc::new(RefCell::new(None));
        let waiting = Rc::new(Cell::new(0));
        let messages = Rc::new(RefCell::new(Vec::new()));
        let saves = Rc::new(RefCell::new(Saves::default()));
        let places = Rc::new(RefCell::new(Places::load(dirs.profile.join("places.json"))));
        let mut session = Session::new();
        session.new_workbook();
        let services = services(&dirs, inbox.clone(), pictures.clone(), waiting.clone(), messages.clone(), saves.clone(), places.clone());
        let mut app = SheetApp::new(session, services);
        // First run: the system's language and light or dark look, until chosen in the app.
        app.ui.language = gridcraft_ui_egui::i18n::Language::system();
        app.ui.system_theme = true;
        let stored_ui = read_small(&dirs.profile.join("ui.json"));
        let stored_prefs = read_small(&dirs.profile.join("prefs.json"));
        if let Some(s) = &stored_ui
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(s)
            && let Ok(state) = serde_json::from_value::<gridcraft_ui_egui::UiState>(json.clone())
        {
            let language = gridcraft_ui_egui::i18n::saved_language(&json).unwrap_or(app.ui.language);
            app.ui = state.at_startup();
            app.ui.language = language;
        }
        if let Some(s) = &stored_prefs
            && let Ok(p) = serde_json::from_str(s)
        {
            app.session.prefs = p;
        }
        SheetApp::setup_context_for_language(ctx, app.ui.dark, app.ui.language);
        app.after_engine();
        let mut copies = WorkCopies::load(dirs.docs.clone(), dirs.profile.join("workcopies.json"));
        let kept = copies.tidy();
        // A workbook whose changes its own file lacks (another app changed the file, or writing
        // failed) opens again, as it was left, with the reason on the notice bar.
        for (path, state) in &kept {
            app.open_path(path);
            app.notice = Some(not_saved_notice(path, *state));
        }
        watch_inbox(dirs.inbox.clone(), ctx.clone());
        Self {
            app,
            dirs,
            inbox,
            pictures,
            touches: Touches::default(),
            waiting,
            keyboard: false,
            press: None,
            tapped_at: None,
            entering: false,
            cell_editing: false,
            ribbon_before_keyboard: None,
            places,
            messages,
            prefs_written: (stored_ui, stored_prefs),
            insets: None,
            light_bars: None,
            copies,
            saves,
            open_copies: kept.into_iter().map(|(p, _)| p).collect(),
            seeded: false,
        }
    }

    /// Deliver what the Java side left in the inbox.
    fn read_inbox(&mut self, raw: &mut egui::RawInput) {
        for message in mailbox::drain(&self.dirs.inbox) {
            match message {
                Message::Opened { name, purpose: Purpose::Workbook, uri, bytes } => self.open_workbook(name, uri, bytes),
                Message::Opened { name, purpose: Purpose::Picture, bytes, .. } => match self.pictures.borrow_mut().take() {
                    Some(inbox) => inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((name, bytes)),
                    None => log::warn!("a picture arrived with nothing waiting for it: {name}"),
                },
                Message::PickerClosed(_) => self.waiting.set(self.waiting.get().saturating_sub(1)),
                Message::Saved { ticket, requested, outcome } => {
                    self.waiting.set(self.waiting.get().saturating_sub(1));
                    let job = self.saves.borrow_mut().jobs.remove(&ticket);
                    if job.as_ref().is_some_and(|j| j.name != requested) {
                        log::warn!("save {ticket}: the answer names {requested}");
                    }
                    match outcome {
                        Outcome::Saved { name, uri } => {
                            log::info!("saved {requested} as {name}");
                            if mailbox::is_workbook_save(&requested)
                                && mailbox::is_workbook_save(&name)
                                && let Some(doc) = job.and_then(|j| j.doc)
                            {
                                self.adopt(doc, &name, &uri);
                            }
                            self.app.toast = Some((format!("Saved {name}"), gridcraft_ui_egui::now_ms()));
                        }
                        Outcome::Cancelled => {}
                        Outcome::Failed(reason) => self.app.message = Some(("GridCraft".into(), format!("Couldn't save {requested}: {reason}"))),
                    }
                }
                Message::Synced { path, outcome } => match self.copies.answered(&path, outcome) {
                    Notice::None => {}
                    Notice::Conflict(name) => {
                        self.app.notice = Some(not_saved_notice(&path, State::Conflict));
                        self.app.message = Some((
                            "GridCraft".into(),
                            format!(
                                "“{name}” was changed by another app since GridCraft opened it, so GridCraft didn't overwrite it. \
                                 Your changes are kept here: use File › Save As to save them as a new file."
                            ),
                        ));
                    }
                    Notice::Failed(name, reason) => {
                        self.app.notice = Some(not_saved_notice(&path, State::Failed));
                        self.app.message = Some((
                            "GridCraft".into(),
                            format!(
                                "Couldn't save the changes to “{name}”: {reason}. GridCraft tries again when you next save; File › Save As saves a copy elsewhere."
                            ),
                        ));
                    }
                },
                Message::Text(text) => raw.events.push(egui::Event::Text(text)),
                Message::Key { code, meta } => raw.events.extend(keys::press(code, meta)),
                Message::Insets { left, top, right, bottom } => {
                    log::info!("window insets: left {left} top {top} right {right} bottom {bottom}");
                    self.insets = Some(egui::epaint::MarginF32 { left, right, top, bottom });
                    // The keyboard came up (or went): the cell being typed into stays in sight.
                    if self.app.editor.is_some() || self.entering {
                        self.app.grid.ensure_visible = true;
                    }
                }
            }
        }
    }

    /// A workbook picked in File › Open, or opened or shared from another app. One the app may
    /// write back to (and that GridCraft saves in its own format) opens from a working copy, so
    /// Save and AutoSave work as on the desktop; any other opens as an untitled copy, which Save
    /// asks where to keep.
    fn open_workbook(&mut self, name: String, uri: Option<String>, bytes: Vec<u8>) {
        log::info!("received {name} ({} bytes, {})", bytes.len(), if uri.is_some() { "writable" } else { "read only" });
        let own_format = matches!(name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref(), Some("xlsx" | "xlsm" | "csv" | "tsv"));
        let Some(uri) = uri.filter(|_| own_format) else {
            self.inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((name, bytes));
            return;
        };
        self.remember(&uri, &name);
        // Already open from the same file: show it rather than a second copy.
        if let Some(i) = self.app.session.documents().iter().position(|d| d.path.as_deref().is_some_and(|p| self.copies.uri(p) == Some(uri.as_str())))
        {
            self.app.session.set_active(i);
            self.app.backstage = None;
            self.app.after_engine();
            return;
        }
        match self.copies.create(&name, &bytes, &uri) {
            Ok(path) => self.app.open_path(&path.to_string_lossy()),
            Err(e) => {
                log::warn!("no working copy for {name}: {e}");
                self.inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((name, bytes));
            }
        }
    }

    /// Save As put workbook `doc` in storage as `name` at `uri`: like Save As on the desktop, the
    /// workbook now has that file (a working copy linked to it), its title says so, and the next
    /// Save writes there.
    fn adopt(&mut self, doc: u64, name: &str, uri: &str) {
        let Some(index) = self.app.session.document_index(doc) else { return };
        let path = match self.copies.new_path(name) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("no working copy for {name}: {e}");
                return;
            }
        };
        let old = self.app.session.documents().get(index).and_then(|d| d.path.clone());
        let active = self.app.session.active_index();
        self.app.session.set_active(index);
        let saved = self.app.session.run("file.save", json!({"path": path.to_string_lossy()}));
        self.app.session.set_active(active);
        match saved {
            Ok(_) => {
                self.copies.link(&path, uri);
                self.remember(uri, name);
                // The workbook's old working copy (and a warning that its file lacked the
                // changes) is done with: the changes are in the new file.
                if let Some(old) = old.filter(|p| self.copies.contains(p)) {
                    self.copies.forget(&old);
                    self.app.notice = None;
                }
            }
            Err(e) => log::warn!("{name} saved, but the workbook keeps its old file: {e}"),
        }
        self.app.after_engine();
    }

    /// A workbook was opened from, or saved to, `uri` as `name`: it heads the Recent list, which
    /// opens it again from there (the app keeps its permission to).
    fn remember(&mut self, uri: &str, name: &str) {
        self.places.borrow_mut().note(uri, name);
        let recent = &mut self.app.ui.recent;
        recent.retain(|r| r != uri);
        recent.insert(0, uri.to_string());
        recent.truncate(20);
        self.places.borrow_mut().keep_only(recent);
    }

    /// Hand saved working copies to the activity, which writes them over their originals; note
    /// which workbook a save just started is a copy of; keep Recent to files this device can open.
    fn after_app(&mut self) {
        let open: Vec<String> = self.app.session.documents().iter().filter_map(|d| d.path.clone()).collect();
        // A workbook closed while its own file lacks its changes: the user was told (and could
        // Save As), so its copy goes rather than coming back at every start.
        for closed in std::mem::take(&mut self.open_copies).into_iter().filter(|p| !open.contains(p)) {
            if matches!(self.copies.state(&closed), Some(State::Conflict | State::Failed)) {
                log::info!("working copies: {closed} closed without its changes reaching its file");
                self.copies.forget(&closed);
            }
        }
        self.open_copies = open.iter().filter(|p| self.copies.contains(p)).cloned().collect();
        // File › Info names the file a working copy stands for.
        for path in &self.open_copies {
            if let Some(uri) = self.copies.uri(path) {
                self.places.borrow_mut().link(path, uri);
            }
        }
        for (path, uri) in self.copies.saved(&open) {
            if let Err(e) = bridge::write_back(&path, &uri) {
                log::warn!("write back {path}: {e}");
                let _ = self.copies.answered(&path, mailbox::Sync::Failed(e));
            }
        }
        let active = self.app.session.active().map(|d| d.uid);
        for job in self.saves.borrow_mut().jobs.values_mut() {
            if job.doc.is_none() {
                job.doc = active;
            }
        }
        // Working copies come and go: Recent lists the files they stand for (`remember`).
        let docs = self.dirs.docs.to_string_lossy().into_owned();
        self.app.ui.recent.retain(|p| !p.starts_with(&docs));
    }

    /// Write the preferences when they changed: Android may stop the app without warning.
    fn store_prefs(&mut self) {
        // The ribbon folded away for the keyboard is not the user's choice.
        let mut state = self.app.ui.clone();
        if let Some(was) = self.ribbon_before_keyboard {
            state.ribbon_collapsed = was;
        }
        let (Ok(ui), Ok(prefs)) = (serde_json::to_string(&state), serde_json::to_string(&self.app.session.prefs)) else { return };
        for (file, json, written) in [("ui.json", ui, &mut self.prefs_written.0), ("prefs.json", prefs, &mut self.prefs_written.1)] {
            if written.as_deref() == Some(json.as_str()) {
                continue;
            }
            let path = self.dirs.profile.join(file);
            let tmp = self.dirs.profile.join(format!(".{file}.tmp"));
            match std::fs::write(&tmp, &json).and_then(|()| std::fs::rename(&tmp, &path)) {
                Ok(()) => *written = Some(json),
                Err(e) => log::warn!("preferences: cannot save {}: {e}", path.display()),
            }
        }
    }

    /// Show the on-screen keyboard when a text field (the cell being edited, the formula bar, a
    /// box in a dialog) takes typing right after a tap on it; keep it up after Enter or Tab ends a
    /// cell's edit (the next cell takes typing, as in Excel); put it away when nothing takes
    /// typing. While it is up for cells, the ribbon folds away to leave the sheet room.
    fn keyboard_follows(&mut self, ctx: &egui::Context, typing_area: Option<egui::Rect>, released: bool, now: f64) {
        if released && let Some(pos) = self.press.take() {
            self.tapped_at = Some((now, pos));
        }
        let tapped_in = |area: egui::Rect| self.tapped_at.is_some_and(|(t, pos)| now - t < 0.8 && area.expand(8.0).contains(pos));
        let (moved_on, pressed) = ctx.input(|i| {
            let moved_on = i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::Enter | egui::Key::Tab, pressed: true, .. }));
            (moved_on, i.pointer.any_pressed())
        });
        match typing_area {
            None => {
                if self.keyboard && self.cell_editing && moved_on && !pressed && self.app.backstage.is_none() {
                    self.entering = true;
                } else if self.keyboard && (!self.entering || pressed || self.app.backstage.is_some()) {
                    self.keyboard = false;
                    self.entering = false;
                    if let Err(e) = bridge::keyboard(false) {
                        log::warn!("keyboard: {e}");
                    }
                }
            }
            Some(area) => {
                self.entering = false;
                let tapped = tapped_in(area);
                if tapped && !self.keyboard {
                    self.keyboard = true;
                    self.tapped_at = None;
                    if let Err(e) = bridge::keyboard(true) {
                        log::warn!("keyboard: {e}");
                    }
                } else if tapped && released {
                    // Tapped again into a field while the keyboard was put away by hand: show it.
                    self.tapped_at = None;
                    if let Err(e) = bridge::keyboard(true) {
                        log::warn!("keyboard: {e}");
                    }
                }
            }
        }
        self.cell_editing = self.app.editor.is_some();
        let fold = self.keyboard && (self.cell_editing || self.entering);
        match (fold, self.ribbon_before_keyboard) {
            (true, None) => {
                self.ribbon_before_keyboard = Some(self.app.ui.ribbon_collapsed);
                self.app.ui.ribbon_collapsed = true;
                self.app.grid.ensure_visible = true;
            }
            (false, Some(was)) => {
                self.ribbon_before_keyboard = None;
                self.app.ui.ribbon_collapsed = was;
            }
            _ => {}
        }
        if self.tapped_at.is_some_and(|(t, _)| now - t < 0.8) {
            // A field may take focus a frame or two after the tap (a double-tap starts editing).
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

impl eframe::App for Shell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.read_inbox(raw);
        // Keep the app clear of the status bar, the navigation bar, a camera cutout and the
        // on-screen keyboard: Android draws apps edge to edge, under all of them.
        if let Some(px) = self.insets {
            let ppp = ctx.pixels_per_point().max(0.1);
            let points = egui::epaint::MarginF32 { left: px.left / ppp, right: px.right / ppp, top: px.top / ppp, bottom: px.bottom / ppp };
            raw.safe_area_insets = Some(egui::SafeAreaInsets(points));
        }
        for event in &mut raw.events {
            match event {
                // The system Back button closes what's open, as Escape does.
                egui::Event::Key { key: key @ egui::Key::BrowserBack, .. } => *key = egui::Key::Escape,
                // Paste takes the system clipboard (another app's copy), when it has text.
                egui::Event::Paste(text) => {
                    match read_text(&self.dirs.clipboard) {
                        Some(system) => *text = system,
                        // Nothing on the system clipboard: never paste the seed.
                        None if text.as_str() == PASTE_SEED => text.clear(),
                        None => {}
                    }
                }
                _ => {}
            }
        }
        // Ctrl+V with nothing on the clipboard pastes nothing (rather than an empty cell).
        raw.events.retain(|e| !matches!(e, egui::Event::Paste(text) if text.is_empty()));
        self.touches.grabs.clone_from(&self.app.touch_grabs);
        if self.touches.process(raw) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        self.app.raw_input_hook(ctx, raw);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let messages = std::mem::take(&mut *self.messages.borrow_mut());
        if let Some(last) = messages.into_iter().last() {
            self.app.message = Some(("GridCraft".into(), last));
        }
        self.app.logic(ctx);
        self.after_app();
        self.store_prefs();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let (pressed_at, released, now) =
            ui.ctx().input(|i| (i.pointer.any_pressed().then(|| i.pointer.press_origin()).flatten(), i.pointer.any_released(), i.time));
        if let Some(pos) = pressed_at {
            self.press = Some(pos);
        }
        // Android draws the app under the status bar, the navigation bar and the keyboard: run it
        // in the part of the window they leave free (`content_rect`, from the insets), over the
        // app's own background.
        let free = ui.ctx().content_rect();
        ui.painter().rect_filled(ui.ctx().viewport_rect(), 0.0, ui.visuals().panel_fill);
        // The status and navigation bars' icons follow the app's theme.
        let light = !ui.visuals().dark_mode;
        if self.light_bars != Some(light) {
            self.light_bars = Some(light);
            if let Err(e) = bridge::light_system_bars(light) {
                log::warn!("system bars: {e}");
            }
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(free), |ui| {
            ui.set_clip_rect(free);
            self.app.ui(ui);
        });
        let (typing_area, copied) = ui.ctx().output(|o| {
            let copied = o.commands.iter().rev().find_map(|c| match c {
                egui::OutputCommand::CopyText(t) if !t.is_empty() && t != PASTE_SEED => Some(t.clone()),
                _ => None,
            });
            (o.ime.as_ref().map(|ime| ime.rect), copied)
        });
        // The shell decides when the keyboard shows, not egui-winit (which would show it whenever
        // a field has focus, also when the app gave it focus without a tap).
        ui.ctx().output_mut(|o| o.ime = None);
        if let Some(text) = copied
            && let Err(e) = bridge::set_clipboard("", &text)
        {
            log::warn!("clipboard: {e}");
        }
        if !self.seeded {
            self.seeded = true;
            ui.ctx().copy_text(PASTE_SEED.to_string());
        }
        self.keyboard_follows(&ui.ctx().clone(), typing_area, released, now);
        self.after_app();
    }

    fn on_exit(&mut self) {
        if self.waiting.get() > 0 {
            log::info!("closing with {} picker or save answers outstanding", self.waiting.get());
        }
        self.store_prefs();
    }
}

/// The notice bar for a workbook whose own file lacks its saved changes (`state`: conflict or
/// failed), with Save As at hand.
fn not_saved_notice(path: &str, state: State) -> gridcraft_ui_egui::Notice {
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let text = if state == State::Conflict {
        format!("Not saved to “{name}”: another app changed that file. Your changes are kept here; save them with Save As.")
    } else {
        format!("Not saved to “{name}” yet: writing to that file failed. Save again, or keep a copy with Save As.")
    };
    gridcraft_ui_egui::Notice { text, kind: gridcraft_ui_egui::NoticeKind::Warning, action: Some(("Save As…".into(), "saveAs".into())) }
}

/// A small text file (preferences, the clipboard mirror), if it exists and is under 16 MB.
fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 16 << 20 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The clipboard mirror's text (the activity refreshes it whenever the app comes to the front or
/// the clipboard changes; Android lets only the focused app read it). `None` when empty.
fn read_text(path: &Path) -> Option<String> {
    read_small(path).filter(|t| !t.is_empty())
}

/// Wake the app whenever the Java side leaves a message: its answers arrive while the app may be
/// idle, waiting for input. A glance at the folder every few milliseconds costs next to nothing.
fn watch_inbox(dir: PathBuf, ctx: egui::Context) {
    let spawned = std::thread::Builder::new().name("inbox".into()).spawn(move || {
        loop {
            if mailbox::has_mail(&dir) {
                ctx.request_repaint();
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    });
    if let Err(e) = spawned {
        log::warn!("inbox watcher: {e}");
    }
}

fn clear_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if let Err(err) = std::fs::remove_file(e.path()) {
            log::warn!("cannot remove {}: {err}", e.path().display());
        }
    }
}

fn services(
    dirs: &Dirs,
    inbox: Inbox,
    pictures: Rc<RefCell<Option<Inbox>>>,
    waiting: Rc<Cell<u32>>,
    messages: Rc<RefCell<Vec<String>>>,
    saves: Rc<RefCell<Saves>>,
    places: Rc<RefCell<Places>>,
) -> Services {
    let (open_waiting, picture_waiting, recent_waiting) = (waiting.clone(), waiting.clone(), waiting.clone());
    let (open_messages, picture_messages, save_messages, recent_messages) = (messages.clone(), messages.clone(), messages.clone(), messages.clone());
    let outbox = dirs.outbox.clone();
    let print_outbox = dirs.outbox.clone();
    let clipboard = dirs.clipboard.clone();
    let clipboard_html = dirs.clipboard_html.clone();
    Services {
        open_async: Some(Box::new(move || match bridge::open(Purpose::Workbook.as_str()) {
            Ok(()) => open_waiting.set(open_waiting.get().saturating_add(1)),
            Err(e) => {
                log::error!("cannot show the file picker: {e}");
                open_messages.borrow_mut().push(format!("Couldn't show the file picker: {e}"));
            }
        })),
        pick_picture_async: Some(Box::new(move |target: Inbox| {
            *pictures.borrow_mut() = Some(target);
            match bridge::open(Purpose::Picture.as_str()) {
                Ok(()) => picture_waiting.set(picture_waiting.get().saturating_add(1)),
                Err(e) => {
                    log::error!("cannot show the picture picker: {e}");
                    picture_messages.borrow_mut().push(format!("Couldn't show the file picker: {e}"));
                }
            }
        })),
        download: Some(Box::new(move |name: &str, bytes: &[u8]| {
            if let Err(e) = save(&outbox, &saves, &waiting, name, bytes) {
                log::error!("save {name}: {e}");
                save_messages.borrow_mut().push(format!("Couldn't save {name}: {e}"));
            }
        })),
        print: Some(Box::new(move |bytes: &[u8]| {
            let file = print_outbox.join("print.pdf");
            if let Err(e) =
                std::fs::write(&file, bytes).map_err(|e| e.to_string()).and_then(|()| bridge::print(&file.to_string_lossy(), "GridCraft workbook"))
            {
                log::error!("print: {e}");
            }
        })),
        copy_html: Some(Box::new(|html: &str, text: &str| bridge::set_clipboard(html, text))),
        paste_html: Some(Box::new(move || read_text(&clipboard_html))),
        paste_text: Some(Box::new(move || read_text(&clipboard))),
        open_url: Some(Box::new(|url: &str| {
            // Only web and mail links leave the app (as on the desktop).
            let gridcraft_engine::links::LinkKind::External(url) = gridcraft_engine::links::classify(url) else {
                log::warn!("refused to open {url:?}: only http, https and mailto links are opened");
                return;
            };
            if let Err(e) = bridge::open_url(&url) {
                log::warn!("open {url}: {e}");
            }
        })),
        inbox: Some(inbox),
        place: Some(Box::new(move |path: &str| places.borrow().get(path))),
        open_recent: Some(Box::new(move |entry: &str| {
            if !entry.starts_with("content://") {
                return false;
            }
            match bridge::open_recent(entry) {
                Ok(()) => recent_waiting.set(recent_waiting.get().saturating_add(1)),
                Err(e) => {
                    log::error!("cannot open {entry}: {e}");
                    recent_messages.borrow_mut().push(format!("Couldn't open the workbook: {e}"));
                }
            }
            true
        })),
        ..Default::default()
    }
}

/// Hand `bytes` to the activity's save picker as `name`. The answer comes later, through the
/// inbox, under the save's ticket.
fn save(outbox: &Path, saves: &Rc<RefCell<Saves>>, waiting: &Rc<Cell<u32>>, name: &str, bytes: &[u8]) -> Result<(), String> {
    let name = mailbox::safe_name(name);
    let ticket = {
        let mut s = saves.borrow_mut();
        s.next = s.next.saturating_add(1);
        s.next
    };
    let file = outbox.join(format!("{ticket}-{name}"));
    std::fs::write(&file, bytes).map_err(|e| format!("cannot stage the file: {e}"))?;
    if let Err(e) = bridge::save(ticket, &file.to_string_lossy(), &name, mailbox::mime_for(&name)) {
        let _ = std::fs::remove_file(&file);
        return Err(e);
    }
    saves.borrow_mut().jobs.insert(ticket, SaveJob { name, doc: None });
    waiting.set(waiting.get().saturating_add(1));
    Ok(())
}
