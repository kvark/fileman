use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

use fileman::{app_state, core, snapshot, theme, workers};

use crate::input;
use crate::replay::{
    BrowserModeDump, EntryDump, FileAssert, FsAssert, FsCheckMode, FsEntryKind, PanelAssert,
    PanelDump, ReplayAsserts, ReplayKey, SnapshotAssert, StateDump, load_replay_case,
};
use crate::snapshot_render::render_snapshot;
use crate::{
    HighlightRequest, HighlightResult, ImageCache, ImageRequest, SNAPSHOT_HEIGHT, SNAPSHOT_WIDTH,
    ScrollMode, UiCache, UiRender, draw_root_ui, load_fs_directory_async, pump_async,
    refresh_fs_panels,
};

fn parse_modifiers(raw: &[String]) -> egui::Modifiers {
    let mut mods = egui::Modifiers::NONE;
    for item in raw {
        match item.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= egui::Modifiers::CTRL,
            "alt" => mods |= egui::Modifiers::ALT,
            "shift" => mods |= egui::Modifiers::SHIFT,
            _ => {}
        }
    }
    mods
}

fn parse_key_name(name: &str) -> Option<egui::Key> {
    match name.to_ascii_lowercase().as_str() {
        "enter" => Some(egui::Key::Enter),
        "tab" => Some(egui::Key::Tab),
        "escape" | "esc" => Some(egui::Key::Escape),
        "backspace" => Some(egui::Key::Backspace),
        "up" => Some(egui::Key::ArrowUp),
        "down" => Some(egui::Key::ArrowDown),
        "left" => Some(egui::Key::ArrowLeft),
        "right" => Some(egui::Key::ArrowRight),
        "pageup" | "pgup" => Some(egui::Key::PageUp),
        "pagedown" | "pgdn" => Some(egui::Key::PageDown),
        "home" => Some(egui::Key::Home),
        "end" => Some(egui::Key::End),
        "space" => Some(egui::Key::Space),
        "insert" | "ins" => Some(egui::Key::Insert),
        "delete" | "del" => Some(egui::Key::Delete),
        "f1" => Some(egui::Key::F1),
        "f2" => Some(egui::Key::F2),
        "f3" => Some(egui::Key::F3),
        "f4" => Some(egui::Key::F4),
        "f5" => Some(egui::Key::F5),
        "f6" => Some(egui::Key::F6),
        "f7" => Some(egui::Key::F7),
        "f8" => Some(egui::Key::F8),
        "f9" => Some(egui::Key::F9),
        "f10" => Some(egui::Key::F10),
        "f11" => Some(egui::Key::F11),
        "f12" => Some(egui::Key::F12),
        // Single-letter keys (for Ctrl+<letter> combos)
        "a" => Some(egui::Key::A),
        "b" => Some(egui::Key::B),
        "c" => Some(egui::Key::C),
        "d" => Some(egui::Key::D),
        "e" => Some(egui::Key::E),
        "f" => Some(egui::Key::F),
        "g" => Some(egui::Key::G),
        "h" => Some(egui::Key::H),
        "i" => Some(egui::Key::I),
        "j" => Some(egui::Key::J),
        "k" => Some(egui::Key::K),
        "l" => Some(egui::Key::L),
        "m" => Some(egui::Key::M),
        "n" => Some(egui::Key::N),
        "o" => Some(egui::Key::O),
        "p" => Some(egui::Key::P),
        "q" => Some(egui::Key::Q),
        "r" => Some(egui::Key::R),
        "s" => Some(egui::Key::S),
        "t" => Some(egui::Key::T),
        "u" => Some(egui::Key::U),
        "v" => Some(egui::Key::V),
        "w" => Some(egui::Key::W),
        "x" => Some(egui::Key::X),
        "y" => Some(egui::Key::Y),
        "z" => Some(egui::Key::Z),
        _ => None,
    }
}

fn apply_replay_key(
    headless: &mut HeadlessUi,
    app: &mut app_state::AppState,
    ui_cache: &mut UiCache,
    key: &ReplayKey,
) {
    let modifiers = parse_modifiers(&key.modifiers);

    let mut events = Vec::new();
    let key_name = key.key.as_str();
    if key_name.eq_ignore_ascii_case("enter")
        && matches!(
            app.pending_op,
            Some(
                fileman::app_state::PendingOp::Delete { .. }
                    | fileman::app_state::PendingOp::Copy { .. }
                    | fileman::app_state::PendingOp::Move { .. }
            )
        )
    {
        input::confirm_pending_op(app);
        headless.run_frame(app, ui_cache, Vec::new());
        return;
    }
    if key_name.eq_ignore_ascii_case("wait") {
        wait_for_idle(headless, app, ui_cache, 600);
        return;
    }
    if let Some(rest) = key_name.strip_prefix("wait:")
        && let Ok(ms) = rest.trim().parse::<u64>()
    {
        wait_for_duration(
            headless,
            app,
            ui_cache,
            std::time::Duration::from_millis(ms),
        );
        return;
    }
    if let Some(rest) = key_name.strip_prefix("select:") {
        let name = rest.trim();
        let window_rows = match app.active_panel {
            core::ActivePanel::Left => ui_cache.left_rows.max(1),
            core::ActivePanel::Right => ui_cache.right_rows.max(1),
        };
        let panel = app.get_active_panel_mut();
        if let Some(index) = panel.browser.entries.iter().position(|e| e.name == name) {
            app.select_entry(index, window_rows);
        } else {
            let mut sample = String::new();
            for entry in panel.browser.entries.iter().take(8) {
                if !sample.is_empty() {
                    sample.push_str(", ");
                }
                sample.push_str(&entry.name);
            }
            panic!("Replay select failed for \"{name}\". Entries: [{sample}]");
        }
        headless.run_frame(app, ui_cache, Vec::new());
        return;
    }
    if let Some(rest) = key_name.strip_prefix("replace:") {
        let name = rest.trim();
        let panel = app.get_active_panel_mut();
        if let Some(ref mut rename) = panel.browser.inline_rename {
            rename.text = name.to_string();
        } else {
            panic!("Replay replace failed: inline rename is not active");
        }
        headless.run_frame(app, ui_cache, Vec::new());
        return;
    }
    if let Some(rest) = key_name.strip_prefix("text:") {
        events.push(egui::Event::Text(rest.to_string()));
    } else if key_name.len() == 1 && modifiers == egui::Modifiers::NONE {
        events.push(egui::Event::Text(key_name.to_string()));
    } else if let Some(egui_key) = parse_key_name(key_name) {
        events.push(egui::Event::Key {
            key: egui_key,
            pressed: true,
            repeat: false,
            modifiers,
            physical_key: None,
        });
        events.push(egui::Event::Key {
            key: egui_key,
            pressed: false,
            repeat: false,
            modifiers,
            physical_key: None,
        });
    }
    headless.run_frame(app, ui_cache, events);
}

fn is_app_pending(app: &app_state::AppState) -> bool {
    let left = &app.left_panel.browser;
    let right = &app.right_panel.browser;
    let edit_loading = app.edit_panel().map(|edit| edit.loading).unwrap_or(false);
    let search_running = matches!(app.search_status, app_state::SearchStatus::Running(_));
    app.io_in_flight > 0
        || left.loading
        || right.loading
        || left.entries_rx.is_some()
        || right.entries_rx.is_some()
        || left.watching_archive.is_some()
        || right.watching_archive.is_some()
        || edit_loading
        || search_running
        || !app.dir_size_pending.is_empty()
}

fn pump_io(app: &mut app_state::AppState) -> bool {
    let mut completed = 0usize;
    while app.io_rx.try_recv().is_ok() {
        completed += 1;
    }
    if completed > 0 {
        app.on_io_completed(completed);
        refresh_fs_panels(app);
        return true;
    }
    false
}

fn drain_async(app: &mut app_state::AppState, max_iters: usize) {
    for _ in 0..max_iters {
        let changed = pump_async(app) || pump_io(app);
        let pending = is_app_pending(app);
        if !changed && !pending {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

fn wait_for_idle(
    headless: &mut HeadlessUi,
    app: &mut app_state::AppState,
    ui_cache: &mut UiCache,
    max_iters: usize,
) {
    for _ in 0..max_iters {
        let changed = pump_async(app) || pump_io(app);
        headless.run_frame(app, ui_cache, Vec::new());
        if !changed && !is_app_pending(app) && headless.highlight_pending.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

fn wait_for_duration(
    headless: &mut HeadlessUi,
    app: &mut app_state::AppState,
    ui_cache: &mut UiCache,
    duration: std::time::Duration,
) {
    let start = std::time::Instant::now();
    while start.elapsed() < duration {
        let _ = pump_async(app) || pump_io(app);
        headless.run_frame(app, ui_cache, Vec::new());
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

fn init_headless_app(root: Option<PathBuf>) -> anyhow::Result<app_state::AppState> {
    let root = match root {
        Some(root) => root,
        None => std::env::current_dir().expect("current_dir"),
    };
    let (io_tx, io_rx, io_cancel_tx) = workers::start_io_worker();
    let (preview_tx, preview_rx) = workers::start_preview_worker(None);
    let (dir_size_tx, dir_size_rx) = workers::start_dir_size_worker();
    let (search_tx, search_rx) = workers::start_search_worker();
    let (edit_tx, edit_rx) = mpsc::channel::<core::EditLoadRequest>();
    let (edit_res_tx, edit_res_rx) = mpsc::channel::<core::EditLoadResult>();

    thread::spawn(move || {
        while let Ok(req) = edit_rx.recv() {
            let text = match std::fs::read(&req.path) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => text,
                    Err(_) => "Refusing to edit binary file.".to_string(),
                },
                Err(e) => format!("Failed to read file: {e}"),
            };
            let _ = edit_res_tx.send(core::EditLoadResult {
                id: req.id,
                path: req.path,
                text,
            });
        }
    });

    let mut app = app_state::AppState {
        left_panel: app_state::PanelState {
            browser: app_state::BrowserState {
                browser_mode: core::BrowserMode::Fs,
                current_path: root.clone(),
                selected_index: 0,
                entries: Vec::new(),
                entries_rx: None,
                prefer_select_name: None,
                top_index: 0,
                loading: false,
                loading_progress: None,
                container_root: None,
                dir_token: 0,
                history_back: Vec::new(),
                history_forward: Vec::new(),
                inline_rename: None,
                sort_mode: core::SortMode::Name,
                sort_desc: false,
                watching_archive: None,
                index_last_seen: 0,
                marked: std::collections::HashSet::new(),
            },
            mode: app_state::PanelMode::Browser,
        },
        right_panel: app_state::PanelState {
            browser: app_state::BrowserState {
                browser_mode: core::BrowserMode::Fs,
                current_path: root.clone(),
                selected_index: 0,
                entries: Vec::new(),
                entries_rx: None,
                prefer_select_name: None,
                top_index: 0,
                loading: false,
                loading_progress: None,
                container_root: None,
                dir_token: 0,
                history_back: Vec::new(),
                history_forward: Vec::new(),
                inline_rename: None,
                sort_mode: core::SortMode::Name,
                sort_desc: false,
                watching_archive: None,
                index_last_seen: 0,
                marked: std::collections::HashSet::new(),
            },
            mode: app_state::PanelMode::Browser,
        },
        active_panel: core::ActivePanel::Left,
        allow_external_open: false,
        preview_return_focus: None,
        wake: None,
        preview_tx,
        preview_rx,
        preview_request_id: 0,
        io_tx,
        io_rx,
        io_cancel_tx,
        io_in_flight: 0,
        io_cancel_requested: false,
        dir_size_tx,
        dir_size_rx,
        dir_sizes: Default::default(),
        dir_size_pending: Default::default(),
        fs_last_selected_name: Default::default(),
        container_last_selected_name: Default::default(),
        container_dir_cache: Default::default(),
        archive_index: Default::default(),
        props_dialog: None,
        theme: theme::Theme::dark(),
        theme_picker_open: false,
        theme_picker_selected: None,
        pending_op: None,
        rename_input: None,
        rename_focus: false,
        edit_request_id: 0,
        edit_tx,
        edit_rx: edit_res_rx,
        search_query: String::new(),
        search_focus: false,
        search_case: core::SearchCase::Insensitive,
        search_mode: core::SearchMode::Name,
        search_results: Vec::new(),
        search_selected: 0,
        search_request_id: 0,
        search_status: app_state::SearchStatus::Idle,
        search_ui: app_state::SearchUiState::Closed,
        search_tx,
        search_rx,
        refresh_tick: 0,
        update_status: app_state::UpdateStatus::Disabled,
        update_rx: None,
    };
    app.theme
        .load_external_from_dir(std::path::Path::new("./themes"));
    Ok(app)
}

pub(crate) fn run_replay(case_path: &PathBuf, snapshot: Option<PathBuf>) -> anyhow::Result<()> {
    let case = load_replay_case(case_path)?;
    let repo_root = std::env::current_dir()?;
    let root = resolve_case_path(&repo_root, &case.root);
    let mut app = init_headless_app(Some(root.clone()))?;
    let left_root = case
        .left
        .as_ref()
        .map(|p| resolve_case_path(&repo_root, p))
        .unwrap_or_else(|| root.clone());
    let right_root = case
        .right
        .as_ref()
        .map(|p| resolve_case_path(&repo_root, p))
        .unwrap_or_else(|| root.clone());
    load_fs_directory_async(&mut app, left_root, core::ActivePanel::Left, None);
    load_fs_directory_async(&mut app, right_root, core::ActivePanel::Right, None);

    let mut ui_cache = UiCache {
        left_rows: 20,
        right_rows: 20,
        scroll_mode: ScrollMode::Default,
        last_left_selected: 0,
        last_right_selected: 0,
        last_active_panel: core::ActivePanel::Left,
        last_left_dir_token: 0,
        last_right_dir_token: 0,
    };
    let mut headless = HeadlessUi::new();

    for key in case.keys {
        apply_replay_key(&mut headless, &mut app, &mut ui_cache, &key);
        drain_async(&mut app, 50);
    }
    wait_for_idle(&mut headless, &mut app, &mut ui_cache, 600);

    if let Some(path) = snapshot {
        render_snapshot(&mut app, &mut ui_cache, &path)?;
    }
    if let Some(ref dump_path) = case.state_dump {
        let dump_path = resolve_case_path(&repo_root, dump_path);
        write_state_dump(&app, &dump_path)?;
    }
    run_replay_asserts(
        &repo_root,
        root.as_path(),
        &mut app,
        &mut ui_cache,
        &case.asserts,
    )?;
    Ok(())
}

pub(crate) struct HeadlessUi {
    egui_ctx: egui::Context,
    image_cache: ImageCache,
    highlight_cache: HashMap<String, egui::text::LayoutJob>,
    highlight_pending: HashSet<String>,
    image_req_tx: mpsc::Sender<ImageRequest>,
    highlight_req_tx: mpsc::Sender<HighlightRequest>,
    highlight_res_rx: mpsc::Receiver<HighlightResult>,
}

impl HeadlessUi {
    pub(crate) fn new() -> Self {
        let (image_req_tx, _image_req_rx) = mpsc::channel::<ImageRequest>();
        let (highlight_req_tx, highlight_req_rx) = mpsc::channel::<HighlightRequest>();
        let (highlight_res_tx, highlight_res_rx) = mpsc::channel::<HighlightResult>();
        thread::spawn(move || {
            while let Ok(req) = highlight_req_rx.recv() {
                let job = crate::highlight_text_job(&req.text, req.ext.as_deref(), req.theme_kind);
                let _ = highlight_res_tx.send(HighlightResult { key: req.key, job });
            }
        });
        Self {
            egui_ctx: egui::Context::default(),
            image_cache: ImageCache::default(),
            highlight_cache: HashMap::new(),
            highlight_pending: HashSet::new(),
            image_req_tx,
            highlight_req_tx,
            highlight_res_rx,
        }
    }

    pub(crate) fn run_frame(
        &mut self,
        app: &mut app_state::AppState,
        ui_cache: &mut UiCache,
        events: Vec<egui::Event>,
    ) {
        let raw_input = egui::RawInput {
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(SNAPSHOT_WIDTH as f32, SNAPSHOT_HEIGHT as f32),
            )),
            viewports: std::iter::once((
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    native_pixels_per_point: Some(1.0),
                    inner_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::Vec2::new(SNAPSHOT_WIDTH as f32, SNAPSHOT_HEIGHT as f32),
                    )),
                    ..Default::default()
                },
            ))
            .collect(),
            ..Default::default()
        };
        let _ = self.egui_ctx.run(raw_input, |ctx| {
            let input = ctx.input(|i| i.clone());
            input::handle_keyboard(ctx, &input, app, ui_cache);
            draw_root_ui(UiRender {
                ctx,
                app,
                ui_cache,
                image_cache: &mut self.image_cache,
                image_req_tx: &self.image_req_tx,
                highlight_cache: &self.highlight_cache,
                highlight_pending: &mut self.highlight_pending,
                highlight_req_tx: &self.highlight_req_tx,
            });
        });
        while let Ok(res) = self.highlight_res_rx.try_recv() {
            self.highlight_cache.insert(res.key.clone(), res.job);
            self.highlight_pending.remove(&res.key);
        }
    }
}

fn resolve_case_path(base: &Path, path: &PathBuf) -> PathBuf {
    if path.is_absolute() {
        path.clone()
    } else {
        base.join(path)
    }
}

fn collect_fs_entries(
    root: &Path,
    rel: &Path,
    out: &mut HashMap<String, FsEntryKind>,
) -> anyhow::Result<()> {
    let full = root.join(rel);
    if let Ok(read) = std::fs::read_dir(&full) {
        for entry in read {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let name = entry.file_name();
            let child_rel = rel.join(name);
            let rel_string = child_rel.to_string_lossy().replace('\\', "/");
            let kind = if file_type.is_dir() {
                FsEntryKind::Dir
            } else {
                FsEntryKind::File
            };
            out.insert(rel_string, kind);
            if file_type.is_dir() {
                collect_fs_entries(root, &child_rel, out)?;
            }
        }
    }
    Ok(())
}

fn assert_fs(root: &Path, fs: &FsAssert) -> anyhow::Result<()> {
    let mut actual = HashMap::new();
    collect_fs_entries(root, std::path::Path::new(""), &mut actual)?;
    for entry in &fs.entries {
        let expected_kind = entry.kind;
        let rel = entry.path.replace('\\', "/");
        let actual_kind = actual
            .get(&rel)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Missing expected entry \"{}\"", entry.path))?;
        if actual_kind != expected_kind {
            return Err(anyhow::anyhow!(
                "Entry \"{}\" kind mismatch: expected {:?}, got {:?}",
                entry.path,
                expected_kind,
                actual_kind
            ));
        }
    }
    if let FsCheckMode::Exact = fs.mode {
        let expected_count = fs.entries.len();
        let actual_count = actual.len();
        if actual_count != expected_count {
            return Err(anyhow::anyhow!(
                "FS entry count mismatch at {}: expected {}, got {}",
                root.to_string_lossy(),
                expected_count,
                actual_count
            ));
        }
    }
    Ok(())
}

fn assert_files(root: &Path, files: &[FileAssert]) -> anyhow::Result<()> {
    for check in files {
        let path = root.join(&check.path);
        let data = std::fs::read_to_string(&path)
            .map_err(|err| anyhow::anyhow!("Failed to read {}: {err}", path.to_string_lossy()))?;
        if let Some(expected) = check.equals.as_ref() {
            if data != *expected {
                return Err(anyhow::anyhow!("File {} contents mismatch", check.path));
            }
        } else if let Some(expected) = check.contains.as_ref()
            && !data.contains(expected)
        {
            return Err(anyhow::anyhow!("File {} missing expected text", check.path));
        }
    }
    Ok(())
}

fn assert_snapshots(
    base: &Path,
    app: &mut app_state::AppState,
    ui_cache: &mut UiCache,
    snapshots: &[SnapshotAssert],
) -> anyhow::Result<()> {
    for check in snapshots {
        let actual = resolve_case_path(base, &check.path);
        let expected = resolve_case_path(base, &check.expected);
        if let Some(parent) = actual.parent() {
            std::fs::create_dir_all(parent)?;
        }
        render_snapshot(app, ui_cache, &actual)?;
        if !expected.exists() {
            println!(
                "Snapshot reference missing, wrote {}",
                actual.to_string_lossy()
            );
            continue;
        }
        let diff = snapshot::compare_snapshots(
            &actual,
            &expected,
            check.max_channel_diff,
            check.max_pixel_fraction,
        )
        .map_err(|err| anyhow::anyhow!(err))?;
        println!(
            "Snapshot diff: mismatched {} / {} ({:.6}), max channel diff {}",
            diff.mismatched, diff.total, diff.fraction, diff.max_channel_diff
        );
    }
    Ok(())
}

fn container_kind_str(kind: fileman::archive::ContainerKind) -> &'static str {
    use fileman::archive::ContainerKind;
    match kind {
        ContainerKind::Zip => "Zip",
        ContainerKind::Tar => "Tar",
        ContainerKind::TarGz => "TarGz",
        ContainerKind::TarBz2 => "TarBz2",
    }
}

fn build_panel_dump(panel: &app_state::PanelState) -> PanelDump {
    let browser = &panel.browser;
    let mode = match panel.mode {
        app_state::PanelMode::Browser => "Browser",
        app_state::PanelMode::Preview(_) => "Preview",
        app_state::PanelMode::Edit(_) => "Edit",
        app_state::PanelMode::Help(_) => "Help",
    };
    let browser_mode = match &browser.browser_mode {
        core::BrowserMode::Fs => BrowserModeDump::Fs,
        core::BrowserMode::Container {
            kind,
            archive_path,
            cwd,
            ..
        } => BrowserModeDump::Container {
            kind: container_kind_str(*kind).to_string(),
            archive_path: archive_path.clone(),
            cwd: cwd.clone(),
        },
        core::BrowserMode::Search {
            root, query, mode, ..
        } => BrowserModeDump::Search {
            root: root.clone(),
            query: query.clone(),
            mode: match mode {
                core::SearchMode::Name => "Name",
                core::SearchMode::Content => "Content",
            }
            .to_string(),
        },
    };
    let sort_mode = match browser.sort_mode {
        core::SortMode::Name => "Name",
        core::SortMode::Date => "Date",
        core::SortMode::Size => "Size",
        core::SortMode::Raw => "Raw",
    };
    let selected_name = browser
        .entries
        .get(browser.selected_index)
        .map(|e| e.name.clone());
    let entries = browser
        .entries
        .iter()
        .map(|e| {
            let location = match &e.location {
                core::EntryLocation::Fs(p) => format!("{}", p.display()),
                core::EntryLocation::Container {
                    kind, inner_path, ..
                } => format!("{}:{inner_path}", container_kind_str(*kind)),
            };
            EntryDump {
                name: e.name.clone(),
                is_dir: e.is_dir,
                is_symlink: e.is_symlink,
                size: e.size,
                location,
            }
        })
        .collect();
    let mut marked: Vec<String> = browser.marked.iter().cloned().collect();
    marked.sort();
    PanelDump {
        mode: mode.to_string(),
        browser_mode,
        current_path: browser.current_path.clone(),
        selected_index: browser.selected_index,
        selected_name,
        sort_mode: sort_mode.to_string(),
        sort_desc: browser.sort_desc,
        loading: browser.loading,
        marked,
        entries,
    }
}

fn write_state_dump(app: &app_state::AppState, path: &Path) -> anyhow::Result<()> {
    let active_panel = match app.active_panel {
        core::ActivePanel::Left => "Left",
        core::ActivePanel::Right => "Right",
    };
    let search_ui = match app.search_ui {
        app_state::SearchUiState::Closed => "Closed",
        app_state::SearchUiState::Open => "Open",
    };
    let dump = StateDump {
        active_panel: active_panel.to_string(),
        search_ui: search_ui.to_string(),
        search_query: app.search_query.clone(),
        left: build_panel_dump(&app.left_panel),
        right: build_panel_dump(&app.right_panel),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pretty = ron::ser::PrettyConfig::default();
    let text = ron::ser::to_string_pretty(&dump, pretty)?;
    std::fs::write(path, text)?;
    println!("State dump written to {}", path.display());
    Ok(())
}

fn panel_mode_str(panel: &app_state::PanelState) -> &'static str {
    match panel.mode {
        app_state::PanelMode::Browser => "Browser",
        app_state::PanelMode::Preview(_) => "Preview",
        app_state::PanelMode::Edit(_) => "Edit",
        app_state::PanelMode::Help(_) => "Help",
    }
}

fn browser_mode_str(mode: &core::BrowserMode) -> &'static str {
    match mode {
        core::BrowserMode::Fs => "Fs",
        core::BrowserMode::Container { .. } => "Container",
        core::BrowserMode::Search { .. } => "Search",
    }
}

fn assert_panel(
    panel: &app_state::PanelState,
    panel_name: &str,
    expected: &PanelAssert,
) -> anyhow::Result<()> {
    if !expected.entries.is_empty() {
        let actual: Vec<&str> = panel
            .browser
            .entries
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        match expected.mode {
            FsCheckMode::Contains => {
                for name in &expected.entries {
                    if !actual.contains(&name.as_str()) {
                        return Err(anyhow::anyhow!(
                            "{panel_name} panel missing expected entry \"{name}\". Actual: {actual:?}"
                        ));
                    }
                }
            }
            FsCheckMode::Exact => {
                let expected_names: Vec<&str> =
                    expected.entries.iter().map(|s| s.as_str()).collect();
                if actual != expected_names {
                    return Err(anyhow::anyhow!(
                        "{panel_name} panel entries mismatch.\n  Expected: {expected_names:?}\n  Actual:   {actual:?}"
                    ));
                }
            }
        }
    }
    if let Some(ref expected_selected) = expected.selected {
        let actual_selected = panel
            .browser
            .entries
            .get(panel.browser.selected_index)
            .map(|e| e.name.as_str())
            .unwrap_or("<none>");
        if actual_selected != expected_selected {
            return Err(anyhow::anyhow!(
                "{panel_name} panel selected mismatch: expected \"{expected_selected}\", got \"{actual_selected}\""
            ));
        }
    }
    if let Some(ref expected_mode) = expected.browser_mode {
        let actual_mode = browser_mode_str(&panel.browser.browser_mode);
        if actual_mode != expected_mode {
            return Err(anyhow::anyhow!(
                "{panel_name} panel browser_mode mismatch: expected \"{expected_mode}\", got \"{actual_mode}\""
            ));
        }
    }
    if let Some(ref expected_mode) = expected.panel_mode {
        let actual_mode = panel_mode_str(panel);
        if actual_mode != expected_mode {
            return Err(anyhow::anyhow!(
                "{panel_name} panel mode mismatch: expected \"{expected_mode}\", got \"{actual_mode}\""
            ));
        }
    }
    if !expected.marked.is_empty() {
        let mut actual_marked: Vec<&str> =
            panel.browser.marked.iter().map(|s| s.as_str()).collect();
        actual_marked.sort();
        let mut expected_marked: Vec<&str> = expected.marked.iter().map(|s| s.as_str()).collect();
        expected_marked.sort();
        if actual_marked != expected_marked {
            return Err(anyhow::anyhow!(
                "{panel_name} panel marked mismatch.\n  Expected: {expected_marked:?}\n  Actual:   {actual_marked:?}"
            ));
        }
    }
    Ok(())
}

fn run_replay_asserts(
    base: &Path,
    root: &Path,
    app: &mut app_state::AppState,
    ui_cache: &mut UiCache,
    asserts: &ReplayAsserts,
) -> anyhow::Result<()> {
    if let Some(fs) = asserts.fs.as_ref() {
        assert_fs(root, fs)?;
    }
    if !asserts.files.is_empty() {
        assert_files(root, &asserts.files)?;
    }
    if !asserts.snapshots.is_empty() {
        assert_snapshots(base, app, ui_cache, &asserts.snapshots)?;
    }
    if let Some(ref panel_assert) = asserts.left_panel {
        assert_panel(&app.left_panel, "Left", panel_assert)?;
    }
    if let Some(ref panel_assert) = asserts.right_panel {
        assert_panel(&app.right_panel, "Right", panel_assert)?;
    }
    Ok(())
}

pub(crate) fn run_snapshot(path: &PathBuf) -> anyhow::Result<()> {
    let mut app = init_headless_app(None)?;
    let mut ui_cache = UiCache {
        left_rows: 10,
        right_rows: 10,
        scroll_mode: ScrollMode::Default,
        last_left_selected: 0,
        last_right_selected: 0,
        last_active_panel: core::ActivePanel::Left,
        last_left_dir_token: 0,
        last_right_dir_token: 0,
    };
    let cur_dir = std::env::current_dir()?;
    load_fs_directory_async(&mut app, cur_dir.clone(), core::ActivePanel::Left, None);
    load_fs_directory_async(&mut app, cur_dir, core::ActivePanel::Right, None);
    drain_async(&mut app, 50);
    render_snapshot(&mut app, &mut ui_cache, path)
}
