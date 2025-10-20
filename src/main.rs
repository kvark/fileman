use gpui::prelude::*;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read},
    path::{self, Path},
    sync::{Arc, mpsc},
    thread,
};
const VIEW_ROWS: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeKind {
    Dark,
    Light,
}

#[derive(Clone)]
struct Theme {
    kind: ThemeKind,
    external: Vec<(String, ThemeColors)>,
    selected_external: Option<usize>,
}

#[derive(Clone)]
struct ThemeColors {
    divider: gpui::Hsla,
    row_bg_selected_active: gpui::Hsla,
    row_bg_selected_inactive: gpui::Hsla,
    row_fg_selected: gpui::Hsla,
    row_fg_active: gpui::Hsla,
    row_fg_inactive: gpui::Hsla,
    panel_border_active: gpui::Hsla,
    panel_border_inactive: gpui::Hsla,
    header_bg: gpui::Hsla,
    header_fg: gpui::Hsla,
    footer_bg: gpui::Hsla,
    footer_fg: gpui::Hsla,
    preview_bg: gpui::Hsla,
    preview_header_bg: gpui::Hsla,
    preview_header_fg: gpui::Hsla,
    preview_text: gpui::Hsla,
}

impl Theme {
    fn dark() -> Self {
        Self {
            kind: ThemeKind::Dark,
            external: Vec::new(),
            selected_external: None,
        }
    }
    fn light() -> Self {
        Self {
            kind: ThemeKind::Light,
            external: Vec::new(),
            selected_external: None,
        }
    }
    fn set_external(&mut self, themes: Vec<(String, ThemeColors)>) {
        self.external = themes;
        self.selected_external = if self.external.is_empty() {
            None
        } else {
            Some(0)
        };
    }
    fn toggle(&mut self) {
        if !self.external.is_empty() {
            let next = match self.selected_external {
                None => 0,
                Some(i) => (i + 1) % self.external.len(),
            };
            self.selected_external = Some(next);
            return;
        }
        self.kind = match self.kind {
            ThemeKind::Dark => ThemeKind::Light,
            ThemeKind::Light => ThemeKind::Dark,
        };
    }
    fn colors(&self) -> ThemeColors {
        if let Some(i) = self.selected_external {
            return self.external[i].1.clone();
        }
        match self.kind {
            ThemeKind::Dark => ThemeColors {
                divider: gpui::Hsla::from(gpui::Rgba {
                    r: 0.2,
                    g: 0.2,
                    b: 0.2,
                    a: 1.0,
                }),
                row_bg_selected_active: gpui::Hsla::from(gpui::Rgba {
                    r: 0.2,
                    g: 0.4,
                    b: 0.7,
                    a: 1.0,
                }),
                row_bg_selected_inactive: gpui::Hsla::from(gpui::Rgba {
                    r: 0.15,
                    g: 0.3,
                    b: 0.5,
                    a: 1.0,
                }),
                row_fg_selected: gpui::Hsla::from(gpui::Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                }),
                row_fg_active: gpui::Hsla::from(gpui::Rgba {
                    r: 0.9,
                    g: 0.9,
                    b: 0.9,
                    a: 1.0,
                }),
                row_fg_inactive: gpui::Hsla::from(gpui::Rgba {
                    r: 0.7,
                    g: 0.7,
                    b: 0.7,
                    a: 1.0,
                }),
                panel_border_active: gpui::Hsla::from(gpui::Rgba {
                    r: 0.2,
                    g: 0.6,
                    b: 0.9,
                    a: 1.0,
                }),
                panel_border_inactive: gpui::Hsla::from(gpui::Rgba {
                    r: 0.1,
                    g: 0.1,
                    b: 0.1,
                    a: 1.0,
                }),
                header_bg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.75,
                    g: 0.75,
                    b: 0.75,
                    a: 1.0,
                }),
                header_fg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
                footer_bg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.15,
                    g: 0.15,
                    b: 0.15,
                    a: 1.0,
                }),
                footer_fg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.8,
                    g: 0.8,
                    b: 0.8,
                    a: 1.0,
                }),
                preview_bg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.08,
                    g: 0.08,
                    b: 0.08,
                    a: 1.0,
                }),
                preview_header_bg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.2,
                    g: 0.2,
                    b: 0.2,
                    a: 1.0,
                }),
                preview_header_fg: gpui::Hsla::from(gpui::Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                }),
                preview_text: gpui::Hsla::from(gpui::Rgba {
                    r: 0.95,
                    g: 0.95,
                    b: 0.95,
                    a: 1.0,
                }),
            },
            ThemeKind::Light => ThemeColors {
                divider: gpui::Hsla::from(gpui::Rgba {
                    r: 0.85,
                    g: 0.85,
                    b: 0.85,
                    a: 1.0,
                }),
                row_bg_selected_active: gpui::Hsla::from(gpui::Rgba {
                    r: 0.75,
                    g: 0.85,
                    b: 1.0,
                    a: 1.0,
                }),
                row_bg_selected_inactive: gpui::Hsla::from(gpui::Rgba {
                    r: 0.85,
                    g: 0.9,
                    b: 1.0,
                    a: 1.0,
                }),
                row_fg_selected: gpui::Hsla::from(gpui::Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
                row_fg_active: gpui::Hsla::from(gpui::Rgba {
                    r: 0.1,
                    g: 0.1,
                    b: 0.1,
                    a: 1.0,
                }),
                row_fg_inactive: gpui::Hsla::from(gpui::Rgba {
                    r: 0.3,
                    g: 0.3,
                    b: 0.3,
                    a: 1.0,
                }),
                panel_border_active: gpui::Hsla::from(gpui::Rgba {
                    r: 0.2,
                    g: 0.6,
                    b: 0.9,
                    a: 1.0,
                }),
                panel_border_inactive: gpui::Hsla::from(gpui::Rgba {
                    r: 0.8,
                    g: 0.8,
                    b: 0.8,
                    a: 1.0,
                }),
                header_bg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.95,
                    g: 0.95,
                    b: 0.95,
                    a: 1.0,
                }),
                header_fg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.05,
                    g: 0.05,
                    b: 0.05,
                    a: 1.0,
                }),
                footer_bg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.92,
                    g: 0.92,
                    b: 0.92,
                    a: 1.0,
                }),
                footer_fg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.2,
                    g: 0.2,
                    b: 0.2,
                    a: 1.0,
                }),
                preview_bg: gpui::Hsla::from(gpui::Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                }),
                preview_header_bg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.92,
                    g: 0.92,
                    b: 0.92,
                    a: 1.0,
                }),
                preview_header_fg: gpui::Hsla::from(gpui::Rgba {
                    r: 0.1,
                    g: 0.1,
                    b: 0.1,
                    a: 1.0,
                }),
                preview_text: gpui::Hsla::from(gpui::Rgba {
                    r: 0.1,
                    g: 0.1,
                    b: 0.1,
                    a: 1.0,
                }),
            },
        }
    }

    fn load_external_from_dir(&mut self, dir: &std::path::Path) {
        let themes = load_themes_from_dir(dir);
        if !themes.is_empty() {
            self.set_external(themes);
        }
    }
}

#[derive(Deserialize, Clone, Default)]
struct SerializableColor {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

#[derive(Deserialize, Default)]
struct ThemeFileColors {
    divider: Option<SerializableColor>,
    row_bg_selected_active: Option<SerializableColor>,
    row_bg_selected_inactive: Option<SerializableColor>,
    row_fg_selected: Option<SerializableColor>,
    row_fg_active: Option<SerializableColor>,
    row_fg_inactive: Option<SerializableColor>,
    panel_border_active: Option<SerializableColor>,
    panel_border_inactive: Option<SerializableColor>,
    header_bg: Option<SerializableColor>,
    header_fg: Option<SerializableColor>,
    footer_bg: Option<SerializableColor>,
    footer_fg: Option<SerializableColor>,
    preview_bg: Option<SerializableColor>,
    preview_header_bg: Option<SerializableColor>,
    preview_header_fg: Option<SerializableColor>,
    preview_text: Option<SerializableColor>,
}

#[derive(Deserialize, Default)]
struct ThemeFile {
    name: Option<String>,
    colors: Option<ThemeFileColors>,
}

fn rgba_from(c: &SerializableColor) -> gpui::Hsla {
    gpui::Hsla::from(gpui::Rgba {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    })
}

fn merge_colors(base: &ThemeColors, patch: &ThemeFileColors) -> ThemeColors {
    ThemeColors {
        divider: patch
            .divider
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.divider),
        row_bg_selected_active: patch
            .row_bg_selected_active
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.row_bg_selected_active),
        row_bg_selected_inactive: patch
            .row_bg_selected_inactive
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.row_bg_selected_inactive),
        row_fg_selected: patch
            .row_fg_selected
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.row_fg_selected),
        row_fg_active: patch
            .row_fg_active
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.row_fg_active),
        row_fg_inactive: patch
            .row_fg_inactive
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.row_fg_inactive),
        panel_border_active: patch
            .panel_border_active
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.panel_border_active),
        panel_border_inactive: patch
            .panel_border_inactive
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.panel_border_inactive),
        header_bg: patch
            .header_bg
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.header_bg),
        header_fg: patch
            .header_fg
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.header_fg),
        footer_bg: patch
            .footer_bg
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.footer_bg),
        footer_fg: patch
            .footer_fg
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.footer_fg),
        preview_bg: patch
            .preview_bg
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.preview_bg),
        preview_header_bg: patch
            .preview_header_bg
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.preview_header_bg),
        preview_header_fg: patch
            .preview_header_fg
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.preview_header_fg),
        preview_text: patch
            .preview_text
            .as_ref()
            .map(rgba_from)
            .unwrap_or_else(|| base.preview_text),
    }
}

fn parse_theme_bytes(name_hint: &str, bytes: &[u8]) -> Option<(String, ThemeColors)> {
    let dark_base = Theme::dark().colors();
    // Try JSON
    if let Ok(tf) = serde_json::from_slice::<ThemeFile>(bytes) {
        let name = tf.name.unwrap_or_else(|| name_hint.to_string());
        let colors = tf
            .colors
            .map(|c| merge_colors(&dark_base, &c))
            .unwrap_or(dark_base.clone());
        return Some((name, colors));
    }
    // Try YAML
    if let Ok(tf) = serde_yaml::from_slice::<ThemeFile>(bytes) {
        let name = tf.name.unwrap_or_else(|| name_hint.to_string());
        let colors = tf
            .colors
            .map(|c| merge_colors(&dark_base, &c))
            .unwrap_or(dark_base.clone());
        return Some((name, colors));
    }
    // Try TOML
    if let Ok(s) = std::str::from_utf8(bytes) {
        if let Ok(tf) = toml::from_str::<ThemeFile>(s) {
            let name = tf.name.unwrap_or_else(|| name_hint.to_string());
            let colors = tf
                .colors
                .map(|c| merge_colors(&dark_base, &c))
                .unwrap_or(dark_base.clone());
            return Some((name, colors));
        }
    }
    None
}

fn load_themes_from_dir(dir: &std::path::Path) -> Vec<(String, ThemeColors)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name_hint = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("theme")
                .to_string();
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(ext.as_str(), "json" | "yaml" | "yml" | "toml") {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                if let Some((name, colors)) = parse_theme_bytes(&name_hint, &bytes) {
                    out.push((name, colors));
                }
            }
        }
    }
    out
}

#[derive(Clone)]
enum EntryLocation {
    Fs(path::PathBuf),
    Zip {
        archive_path: path::PathBuf,
        inner_path: String, // no leading slash, '' means root
    },
}

#[derive(Clone)]
struct DirEntry {
    name: String,
    is_dir: bool,
    location: EntryLocation,
}

#[derive(Clone, PartialEq)]
enum ActivePanel {
    Left,
    Right,
}

enum PanelMode {
    Fs,
    Zip {
        archive_path: path::PathBuf,
        cwd: String,
    },
}

struct PanelState {
    current_path: path::PathBuf, // For Fs mode: real fs path. For Zip: archive file path.
    mode: PanelMode,
    selected_index: usize,
    entries: Vec<DirEntry>,
    // async population
    entries_rx: Option<mpsc::Receiver<Vec<DirEntry>>>,
    // selection restoration by name
    prefer_select_name: Option<String>,
    // virtual scrolling: first visible row index
    top_index: usize,
    // tracked scroll handle to measure viewport bounds and children
    scroll: gpui::ScrollHandle,
    // anchor to capture viewport bounds each frame
    scroll_anchor: gpui::ScrollAnchor,
}

enum PreviewContent {
    Text(String),
    Image(Arc<Path>),
}

enum IOTask {
    Copy {
        src: path::PathBuf,
        dst_dir: path::PathBuf,
    },
}

// Models
struct FileSystemModel {
    left_panel: PanelState,
    right_panel: PanelState,
    active_panel: ActivePanel,
    preview: Option<PreviewContent>,
    io_tx: mpsc::Sender<IOTask>,

    // remember last selected entry name per directory
    fs_last_selected_name: HashMap<path::PathBuf, String>,
    zip_last_selected_name: HashMap<(path::PathBuf, String), String>,
    theme: Theme,
    theme_picker_open: bool,
    theme_picker_selected: Option<usize>,
}

fn start_io_worker() -> mpsc::Sender<IOTask> {
    let (tx, rx) = mpsc::channel::<IOTask>();
    thread::spawn(move || {
        while let Ok(task) = rx.recv() {
            match task {
                IOTask::Copy { src, dst_dir } => {
                    if let Err(e) = copy_recursively(&src, &dst_dir) {
                        eprintln!("Copy error: {e}");
                    }
                }
            }
        }
    });
    tx
}

fn copy_recursively(src: &Path, dst_dir: &Path) -> io::Result<()> {
    if src.is_dir() {
        let dest = dst_dir.join(src.file_name().unwrap());
        fs::create_dir_all(&dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                copy_recursively(&path, &dest)?;
            } else {
                fs::copy(&path, dest.join(entry.file_name()))?;
            }
        }
    } else {
        let dest = dst_dir.join(src.file_name().unwrap());
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dest)?;
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let cur_dir = std::env::current_dir()?;
    let io_tx = start_io_worker();

    gpui::Application::new().run(move |cx| {
        cx.open_window(
            gpui::WindowOptions {
                focus: true,
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Dual Panel File Manager".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, app| {
                let io_tx_clone = io_tx.clone();
                let fs_entity = app.new(move |_| FileSystemModel {
                    left_panel: PanelState {
                        current_path: cur_dir.clone(),
                        mode: PanelMode::Fs,
                        selected_index: 0,
                        entries: Vec::new(),
                        entries_rx: None,
                        prefer_select_name: None,
                        top_index: 0,
                        scroll: gpui::ScrollHandle::new(),
                        scroll_anchor: gpui::ScrollAnchor::for_handle(gpui::ScrollHandle::new()),
                    },
                    right_panel: PanelState {
                        current_path: cur_dir.clone(),
                        mode: PanelMode::Fs,
                        selected_index: 0,
                        entries: Vec::new(),
                        entries_rx: None,
                        prefer_select_name: None,
                        top_index: 0,
                        scroll: gpui::ScrollHandle::new(),
                        scroll_anchor: gpui::ScrollAnchor::for_handle(gpui::ScrollHandle::new()),
                    },
                    active_panel: ActivePanel::Left,
                    preview: None,
                    io_tx: io_tx_clone.clone(),
                    fs_last_selected_name: HashMap::new(),
                    zip_last_selected_name: HashMap::new(),
                    theme: Theme::dark(),
                    theme_picker_open: false,
                    theme_picker_selected: None,
                });

                // Load initial directories
                app.update_entity(&fs_entity, |model: &mut FileSystemModel, cx| {
                    // Load external themes from ./themes (if present)
                    model
                        .theme
                        .load_external_from_dir(std::path::Path::new("./themes"));

                    model.load_fs_directory_async(
                        model.left_panel.current_path.clone(),
                        ActivePanel::Left,
                        None,
                        cx,
                    );
                    model.load_fs_directory_async(
                        model.right_panel.current_path.clone(),
                        ActivePanel::Right,
                        None,
                        cx,
                    );
                });

                let view = app.new(|cx| FileManagerView {
                    model: fs_entity,
                    focus_handle: {
                        window.focus(&cx.focus_handle());
                        cx.focus_handle().clone()
                    },
                });

                window.activate_window();
                app.activate(true);
                let fh = app.read_entity(&view, |v: &FileManagerView, _| v.focus_handle.clone());
                window.focus(&fh);
                view
            },
        )
        .unwrap();
    });
    Ok(())
}

impl FileSystemModel {
    #[profiling::function]
    fn load_fs_directory_async(
        &mut self,
        path: path::PathBuf,
        target_panel: ActivePanel,
        prefer_name: Option<String>,
        cx: &mut gpui::Context<Self>,
    ) {
        // set initial parent entry for instant UI
        let mut initial: Vec<DirEntry> = Vec::new();
        if path.parent().is_some() {
            initial.push(DirEntry {
                name: "..".to_string(),
                is_dir: true,
                location: EntryLocation::Fs(path.parent().unwrap().to_path_buf()),
            });
        }

        // create channel and start background loader
        let (tx, rx) = mpsc::channel::<Vec<DirEntry>>();
        let path_clone = path.clone();

        // Immediately populate UI with a quick snapshot so the list isn't empty
        if let Ok(mut rd) = fs::read_dir(&path) {
            let mut snapshot: Vec<DirEntry> = Vec::with_capacity(128);
            for _ in 0..128 {
                if let Some(ent) = rd.next() {
                    if let Ok(entry) = ent {
                        let file_name = entry.file_name().to_string_lossy().to_string();
                        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                        snapshot.push(DirEntry {
                            name: file_name,
                            is_dir,
                            location: EntryLocation::Fs(entry.path()),
                        });
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            if !snapshot.is_empty() {
                // Send the initial snapshot synchronously
                let _ = tx.send(snapshot);
            }
            // Continue streaming the rest in background
            thread::spawn(move || {
                // Stream directory entries in chunks to avoid blocking on huge folders
                let chunk = 500usize;
                let mut buf: Vec<DirEntry> = Vec::with_capacity(chunk);
                // Continue from where snapshot loop left off
                if let Ok(mut read_dir) = fs::read_dir(&path_clone) {
                    while let Some(entry_res) = read_dir.next() {
                        if let Ok(entry) = entry_res {
                            let file_name = entry.file_name().to_string_lossy().to_string();
                            if let Ok(file_type) = entry.file_type() {
                                let is_dir = file_type.is_dir();
                                buf.push(DirEntry {
                                    name: file_name,
                                    is_dir,
                                    location: EntryLocation::Fs(entry.path()),
                                });
                            }
                            if buf.len() >= chunk {
                                let _ = tx.send(std::mem::take(&mut buf));
                            }
                        }
                    }
                }
                if !buf.is_empty() {
                    let _ = tx.send(buf);
                }
            });
        } else {
            // Fallback to background streaming if we couldn't read_dir immediately
            thread::spawn(move || {
                let chunk = 500usize;
                let mut buf: Vec<DirEntry> = Vec::with_capacity(chunk);
                if let Ok(mut read_dir) = fs::read_dir(&path_clone) {
                    while let Some(entry_res) = read_dir.next() {
                        if let Ok(entry) = entry_res {
                            let file_name = entry.file_name().to_string_lossy().to_string();
                            if let Ok(file_type) = entry.file_type() {
                                let is_dir = file_type.is_dir();
                                buf.push(DirEntry {
                                    name: file_name,
                                    is_dir,
                                    location: EntryLocation::Fs(entry.path()),
                                });
                            }
                            if buf.len() >= chunk {
                                let _ = tx.send(std::mem::take(&mut buf));
                            }
                        }
                    }
                }
                if !buf.is_empty() {
                    let _ = tx.send(buf);
                }
            });
        }

        let remembered = prefer_name
            .clone()
            .or_else(|| self.fs_last_selected_name.get(&path).cloned());
        let panel_state = self.panel_mut(target_panel);
        panel_state.current_path = path.clone();
        panel_state.mode = PanelMode::Fs;
        panel_state.entries = initial;
        panel_state.selected_index = 0;
        panel_state.top_index = 0;
        panel_state.entries_rx = Some(rx);

        // restore selection by name
        panel_state.prefer_select_name = remembered;

        // request a repaint to begin pumping
        cx.notify();
    }

    #[profiling::function]
    fn load_zip_directory_async(
        &mut self,
        archive_path: path::PathBuf,
        cwd: String,
        target_panel: ActivePanel,
        prefer_name: Option<String>,
        cx: &mut gpui::Context<Self>,
    ) {
        // initial ".." entry
        let mut initial: Vec<DirEntry> = Vec::new();
        if !cwd.is_empty() {
            let parent = cwd
                .trim_end_matches('/')
                .rsplit_once('/')
                .map(|(p, _)| p.to_string())
                .unwrap_or_else(|| "".to_string());
            initial.push(DirEntry {
                name: "..".into(),
                is_dir: true,
                location: EntryLocation::Zip {
                    archive_path: archive_path.clone(),
                    inner_path: parent,
                },
            });
        } else {
            if let Some(parent) = archive_path.parent() {
                initial.push(DirEntry {
                    name: "..".into(),
                    is_dir: true,
                    location: EntryLocation::Fs(parent.to_path_buf()),
                });
            }
        }

        let (tx, rx) = mpsc::channel::<Vec<DirEntry>>();
        let ap = archive_path.clone();
        let cwd_clone = cwd.clone();

        // Send a small initial batch synchronously to avoid an empty view
        match Self::read_zip_directory(&ap, &cwd_clone) {
            Ok(mut all) => {
                if !all.is_empty() && all[0].name == ".." {
                    all.remove(0);
                }
                let initial = all.iter().take(128).cloned().collect::<Vec<_>>();
                if !initial.is_empty() {
                    let _ = tx.send(initial);
                }
                // Stream the remaining in background
                thread::spawn(move || {
                    let chunk = 500usize;
                    let mut start = 128.min(all.len());
                    while start < all.len() {
                        let end = (start + chunk).min(all.len());
                        let _ = tx.send(all[start..end].to_vec());
                        start = end;
                    }
                });
            }
            Err(_) => {
                // Nothing to show initially, background attempt won't help since listing failed
            }
        }

        let remembered = prefer_name.clone().or_else(|| {
            self.zip_last_selected_name
                .get(&(archive_path.clone(), cwd.clone()))
                .cloned()
        });
        let panel_state = self.panel_mut(target_panel);

        panel_state.current_path = archive_path.clone();
        panel_state.mode = PanelMode::Zip {
            archive_path: archive_path.clone(),
            cwd: cwd.clone(),
        };
        panel_state.entries = initial;
        panel_state.selected_index = 0;
        panel_state.top_index = 0;
        panel_state.entries_rx = Some(rx);
        panel_state.prefer_select_name = remembered;

        cx.notify();
    }

    #[profiling::function]
    fn read_fs_directory(path: &path::Path) -> anyhow::Result<Vec<DirEntry>> {
        let mut entries = Vec::new();

        let mut read_dir = fs::read_dir(path)?;
        let mut dir_entries = Vec::new();

        while let Some(entry) = read_dir.next() {
            let entry = entry?;
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy().to_string();

            let file_type = entry.file_type()?;
            let is_dir = file_type.is_dir();

            dir_entries.push(DirEntry {
                name: file_name,
                is_dir,
                location: EntryLocation::Fs(entry.path()),
            });
        }

        // Keep sorting in the background loader; we will remove the parent placeholder there.
        dir_entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

        // Include parent here as well for non-async call sites, though async path strips it.
        if path.parent().is_some() {
            entries.push(DirEntry {
                name: "..".to_string(),
                is_dir: true,
                location: EntryLocation::Fs(path.parent().unwrap().to_path_buf()),
            });
        }

        entries.extend(dir_entries);

        Ok(entries)
    }

    #[profiling::function]
    fn read_zip_directory(archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
        let file = fs::File::open(archive_path)?;
        let mut zip = zip::ZipArchive::new(file)?;
        let mut dirs: HashSet<String> = HashSet::new();
        let mut files: Vec<String> = Vec::new();

        let prefix = if cwd.is_empty() {
            "".to_string()
        } else {
            format!("{}/", cwd.trim_end_matches('/'))
        };

        for i in 0..zip.len() {
            let entry = zip.by_index(i)?;
            let name = entry.name();
            if !name.starts_with(&prefix) {
                continue;
            }
            let rem = &name[prefix.len()..];
            if rem.is_empty() {
                continue;
            }
            if let Some(slash) = rem.find('/') {
                let dir = rem[..slash].to_string();
                dirs.insert(dir);
            } else {
                files.push(rem.to_string());
            }
        }

        let mut entries: Vec<DirEntry> = Vec::new();

        // Parent entry
        if !cwd.is_empty() {
            let parent = cwd
                .trim_end_matches('/')
                .rsplit_once('/')
                .map(|(p, _)| p.to_string())
                .unwrap_or_else(|| "".to_string());
            entries.push(DirEntry {
                name: "..".into(),
                is_dir: true,
                location: EntryLocation::Zip {
                    archive_path: archive_path.to_path_buf(),
                    inner_path: parent,
                },
            });
        } else {
            // leaving the archive to its parent FS directory
            if let Some(parent) = archive_path.parent() {
                entries.push(DirEntry {
                    name: "..".into(),
                    is_dir: true,
                    location: EntryLocation::Fs(parent.to_path_buf()),
                });
            }
        }

        let mut dir_entries: Vec<DirEntry> = dirs
            .into_iter()
            .map(|d| DirEntry {
                name: d.clone(),
                is_dir: true,
                location: EntryLocation::Zip {
                    archive_path: archive_path.to_path_buf(),
                    inner_path: if cwd.is_empty() {
                        d
                    } else {
                        format!("{}/{}", cwd.trim_end_matches('/'), d)
                    },
                },
            })
            .collect();

        let mut file_entries: Vec<DirEntry> = files
            .into_iter()
            .map(|f| DirEntry {
                name: f.clone(),
                is_dir: false,
                location: EntryLocation::Zip {
                    archive_path: archive_path.to_path_buf(),
                    inner_path: if cwd.is_empty() {
                        f
                    } else {
                        format!("{}/{}", cwd.trim_end_matches('/'), f)
                    },
                },
            })
            .collect();

        dir_entries.sort_by(|a, b| a.name.cmp(&b.name));
        file_entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries.extend(dir_entries);
        entries.extend(file_entries);

        Ok(entries)
    }

    fn panel(&self, which: ActivePanel) -> &PanelState {
        match which {
            ActivePanel::Left => &self.left_panel,
            ActivePanel::Right => &self.right_panel,
        }
    }

    fn panel_mut(&mut self, which: ActivePanel) -> &mut PanelState {
        match which {
            ActivePanel::Left => &mut self.left_panel,
            ActivePanel::Right => &mut self.right_panel,
        }
    }

    fn get_active_panel(&self) -> &PanelState {
        self.panel(self.active_panel.clone())
    }

    fn get_active_panel_mut(&mut self) -> &mut PanelState {
        self.panel_mut(self.active_panel.clone())
    }

    fn select_entry(&mut self, index: usize) {
        let panel = self.get_active_panel_mut();
        if index < panel.entries.len() {
            panel.selected_index = index;
            // keep cursor visible within the virtual window; only scroll if selection goes out of view
            let window_rows = compute_window_rows(panel);
            if panel.selected_index < panel.top_index {
                panel.top_index = panel.selected_index;
            } else if panel.selected_index >= panel.top_index + window_rows {
                panel.top_index = panel.selected_index + 1 - window_rows;
            }
            if self.preview.is_some() {
                self.update_preview_for_current_selection();
            }
        } else {
            log::error!("Unable to select entry at index {}", index);
        }
    }

    fn open_selected(&mut self, cx: &mut gpui::Context<Self>) {
        let active = self.active_panel.clone();

        // Gather needed data without holding immutable borrows across mutations
        let (selected_entry, current_path, zip_cwd) = {
            let panel = self.get_active_panel();
            if panel.entries.is_empty() {
                return;
            }
            let entry = panel.entries[panel.selected_index].clone();
            let current_path = panel.current_path.clone();
            let zip_cwd = match &panel.mode {
                PanelMode::Zip { cwd, .. } => Some(cwd.clone()),
                _ => None,
            };
            (entry, current_path, zip_cwd)
        };

        // Remember the selection for the current location
        self.store_current_selection_memory();

        match &selected_entry.location {
            EntryLocation::Fs(path) => {
                if selected_entry.is_dir {
                    let prefer_name = if selected_entry.name == ".." {
                        current_path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                    } else {
                        None
                    };

                    self.load_fs_directory_async(path.clone(), active.clone(), prefer_name, cx);

                    if selected_entry.name != ".." {
                        if let Some(name) = self.fs_last_selected_name.get(path).cloned() {
                            self.select_entry_by_name(active, &name);
                        }
                    }
                } else if is_zip_path(path) {
                    self.load_zip_directory_async(path.clone(), "".to_string(), active, None, cx);
                }
            }
            EntryLocation::Zip {
                archive_path,
                inner_path,
            } => {
                if selected_entry.is_dir {
                    let prefer_name = if selected_entry.name == ".." {
                        zip_cwd.as_ref().and_then(|cwd| {
                            cwd.trim_end_matches('/')
                                .rsplit('/')
                                .next()
                                .map(|s| s.to_string())
                        })
                    } else {
                        None
                    };

                    self.load_zip_directory_async(
                        archive_path.clone(),
                        inner_path.clone(),
                        active.clone(),
                        prefer_name,
                        cx,
                    );

                    if selected_entry.name != ".." {
                        if let Some(name) = self
                            .zip_last_selected_name
                            .get(&(archive_path.clone(), inner_path.clone()))
                            .cloned()
                        {
                            self.select_entry_by_name(active, &name);
                        }
                    }
                }
            }
        }
    }

    fn switch_panel(&mut self) {
        self.active_panel = match self.active_panel {
            ActivePanel::Left => ActivePanel::Right,
            ActivePanel::Right => ActivePanel::Left,
        };
    }

    fn store_current_selection_memory(&mut self) {
        let (fs_key, zip_key, selected_name_opt) = {
            let panel = self.get_active_panel();
            if panel.entries.is_empty() {
                return;
            }
            let selected_name = panel.entries[panel.selected_index].name.clone();
            match &panel.mode {
                PanelMode::Fs => (Some(panel.current_path.clone()), None, Some(selected_name)),
                PanelMode::Zip {
                    archive_path, cwd, ..
                } => (
                    None,
                    Some((archive_path.clone(), cwd.clone())),
                    Some(selected_name),
                ),
            }
        };
        if let Some(selected_name) = selected_name_opt {
            if let Some(path) = fs_key {
                self.fs_last_selected_name.insert(path, selected_name);
            } else if let Some((ap, cwd)) = zip_key {
                self.zip_last_selected_name.insert((ap, cwd), selected_name);
            }
        }
    }

    fn select_entry_by_name(&mut self, which: ActivePanel, name: &str) {
        let panel = self.panel_mut(which);
        if let Some(idx) = panel.entries.iter().position(|e| e.name == name) {
            panel.selected_index = idx;
        }
    }

    fn update_preview_for_current_selection(&mut self) {
        let panel = self.get_active_panel();
        if panel.entries.is_empty() {
            self.preview = None;
            return;
        }
        let entry = &panel.entries[panel.selected_index];
        if entry.is_dir {
            self.preview = None;
            return;
        }
        const MAX_BYTES: usize = 64 * 1024;
        match &entry.location {
            EntryLocation::Fs(path) => {
                if is_image_path(path) {
                    self.preview = Some(PreviewContent::Image(Arc::from(path.clone())));
                } else {
                    match read_bytes_prefix(path, MAX_BYTES) {
                        Ok(bytes) => {
                            if is_probably_text(&bytes) {
                                let text = String::from_utf8_lossy(&bytes).into_owned();
                                self.preview = Some(PreviewContent::Text(text));
                            } else {
                                let dump = hexdump(&bytes);
                                self.preview = Some(PreviewContent::Text(dump));
                            }
                        }
                        Err(e) => {
                            self.preview =
                                Some(PreviewContent::Text(format!("Failed to read file: {e}")));
                        }
                    }
                }
            }
            EntryLocation::Zip {
                archive_path,
                inner_path,
            } => match read_zip_bytes_prefix(archive_path, inner_path, MAX_BYTES) {
                Ok(bytes) => {
                    if is_probably_text(&bytes) {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        self.preview = Some(PreviewContent::Text(text));
                    } else {
                        let dump = hexdump(&bytes);
                        self.preview = Some(PreviewContent::Text(dump));
                    }
                }
                Err(e) => {
                    self.preview = Some(PreviewContent::Text(format!(
                        "Failed to read zip entry: {e}"
                    )));
                }
            },
        }
    }

    fn toggle_preview(&mut self) {
        if self.preview.is_some() {
            self.preview = None;
            return;
        }
        self.update_preview_for_current_selection();
    }

    fn enqueue_copy_selected(&mut self) {
        let src = {
            let p = self.get_active_panel();
            if p.entries.is_empty() {
                return;
            }
            match &p.entries[p.selected_index].location {
                EntryLocation::Fs(path) => path.clone(),
                EntryLocation::Zip { .. } => {
                    // Skip copy for zip-internal entries for now
                    return;
                }
            }
        };

        let dst_dir = {
            let other_panel = match self.active_panel {
                ActivePanel::Left => &self.right_panel,
                ActivePanel::Right => &self.left_panel,
            };
            match &other_panel.mode {
                PanelMode::Fs => other_panel.current_path.clone(),
                PanelMode::Zip { .. } => {
                    // Can't copy into zip for now
                    return;
                }
            }
        };

        if let Err(e) = self.io_tx.send(IOTask::Copy {
            src: src.clone(),
            dst_dir: dst_dir.clone(),
        }) {
            eprintln!("Failed to enqueue copy: {e}");
        } else {
            log::info!(
                "Enqueued copy: {} -> {}",
                src.to_string_lossy(),
                dst_dir.to_string_lossy()
            );
        }
    }
    fn switch_theme(&mut self) {
        // If external themes exist and picker is open, apply selected; otherwise toggle
        if self.theme.selected_external.is_some() && self.theme_picker_open {
            self.apply_selected_theme();
        } else {
            self.theme.toggle();
        }
    }

    fn open_theme_picker(&mut self) {
        self.theme_picker_open = true;
        // initialize selection to current external selection or first
        self.theme_picker_selected = self.theme.selected_external.or(Some(0));
    }

    fn close_theme_picker(&mut self) {
        self.theme_picker_open = false;
    }

    fn select_next_theme(&mut self) {
        if self.theme.external.is_empty() {
            return;
        }
        let len = self.theme.external.len();
        let cur = self.theme_picker_selected.unwrap_or(0);
        self.theme_picker_selected = Some((cur + 1) % len);
    }

    fn select_prev_theme(&mut self) {
        if self.theme.external.is_empty() {
            return;
        }
        let len = self.theme.external.len();
        let cur = self.theme_picker_selected.unwrap_or(0);
        self.theme_picker_selected = Some((cur + len - 1) % len);
    }

    fn apply_selected_theme(&mut self) {
        if let Some(i) = self.theme_picker_selected {
            if i < self.theme.external.len() {
                self.theme.selected_external = Some(i);
            }
        }
        self.theme_picker_open = false;
    }

    fn theme_names(&self) -> Vec<String> {
        if self.theme.external.is_empty() {
            vec!["Dark".to_string(), "Light".to_string()]
        } else {
            self.theme.external.iter().map(|(n, _)| n.clone()).collect()
        }
    }
}

fn is_zip_path(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()),
        Some(ext) if ext == "zip"
    )
}

fn is_image_path(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()),
        Some(ext) if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp")
    )
}

fn is_text_path(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()),
        Some(ext)
            if matches!(
                ext.as_str(),
                "txt" | "md" | "json" | "toml" | "yaml" | "yml" | "rs" | "log" | "ini" | "csv"
            )
    )
}

fn read_text_preview(path: &Path, max_bytes: usize) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.by_ref().take(max_bytes as u64).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn read_bytes_prefix(path: &Path, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.by_ref().take(max_bytes as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

fn read_zip_bytes_prefix(
    archive_path: &Path,
    inner_path: &str,
    max_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let file = fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let normalized = inner_path.trim_start_matches('/');
    let mut data = Vec::new();
    let mut found = None;
    for i in 0..zip.len() {
        let name = zip.by_index(i)?.name().to_string();
        if name == normalized {
            found = Some(i);
            break;
        }
    }
    if let Some(idx) = found {
        let mut zf = zip.by_index(idx)?;
        zf.by_ref().take(max_bytes as u64).read_to_end(&mut data)?;
        Ok(data)
    } else {
        Err(anyhow::anyhow!(format!(
            "Entry not found in zip: {}",
            inner_path
        )))
    }
}

fn hexdump(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut offset = 0usize;
    for chunk in bytes.chunks(16) {
        out.push_str(&format!("{:08x}: ", offset));
        for i in 0..16 {
            if i < chunk.len() {
                out.push_str(&format!("{:02x} ", chunk[i]));
            } else {
                out.push_str("   ");
            }
            if i == 7 {
                out.push(' ');
            }
        }
        out.push(' ');
        for &b in chunk {
            let ch = if (0x20..=0x7e).contains(&b) {
                b as char
            } else {
                '.'
            };
            out.push(ch);
        }
        out.push('\n');
        offset += 16;
    }
    out
}

fn is_probably_text(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    // If any NUL bytes, it's likely binary
    if bytes.iter().any(|&b| b == 0) {
        return false;
    }
    // Count printable bytes plus common whitespace
    let mut printable = 0usize;
    for &b in bytes {
        match b {
            0x09 | 0x0A | 0x0D => printable += 1, // tab, LF, CR
            0x20..=0x7E => printable += 1,        // visible ASCII
            _ => {}
        }
    }
    let ratio = printable as f32 / bytes.len().max(1) as f32;
    ratio > 0.85
}

// Views
struct FileManagerView {
    model: gpui::Entity<FileSystemModel>,
    focus_handle: gpui::FocusHandle,
}

impl gpui::Focusable for FileManagerView {
    fn focus_handle(&self, _app: &gpui::App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EventEmitter<gpui::DismissEvent> for FileManagerView {}

impl gpui::Render for FileManagerView {
    #[profiling::function]
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        gpui::div()
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .child(
                gpui::div()
                    .flex_1()
                    .size_full()
                    .min_w(gpui::px(0.0))
                    .child(self.render_panel(ActivePanel::Left, cx)),
            )
            .child(
                gpui::div()
                    .w(gpui::px(2.0))
                    .bg(self.model.read(cx).theme.colors().divider)
                    .h_full(),
            )
            .child(
                gpui::div()
                    .flex_1()
                    .size_full()
                    .min_w(gpui::px(0.0))
                    .child(self.render_panel(ActivePanel::Right, cx)),
            )
            .child(self.render_theme_picker(cx))
            .key_context("parent")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(
                |this: &mut Self,
                 event: &gpui::KeyDownEvent,
                 _window,
                 cx: &mut gpui::Context<Self>| {
                    let key = event.keystroke.key.as_str();
                    let handled = match key {
                        "tab" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                model.switch_panel();
                                if model.preview.is_some() {
                                    model.update_preview_for_current_selection();
                                }
                            });
                            true
                        }
                        "enter" => {
                            this.model.update(cx, |model: &mut FileSystemModel, cx| {
                                if model.theme_picker_open {
                                    model.apply_selected_theme();
                                } else {
                                    model.open_selected(cx);
                                }
                            });
                            true
                        }
                        "down" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                if model.theme_picker_open {
                                    model.select_next_theme();
                                } else {
                                    let panel = model.get_active_panel();
                                    if panel.selected_index + 1 < panel.entries.len() {
                                        model.select_entry(panel.selected_index + 1);
                                    }
                                }
                            });
                            true
                        }
                        "up" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                if model.theme_picker_open {
                                    model.select_prev_theme();
                                } else {
                                    let panel = model.get_active_panel();
                                    if panel.selected_index > 0 {
                                        model.select_entry(panel.selected_index - 1);
                                    }
                                }
                            });
                            true
                        }
                        "f3" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                model.toggle_preview();
                            });
                            true
                        }
                        "escape" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                if model.theme_picker_open {
                                    model.close_theme_picker();
                                } else {
                                    model.preview = None;
                                }
                            });
                            true
                        }
                        "f5" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                model.enqueue_copy_selected();
                            });
                            true
                        }
                        "f9" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                model.switch_theme();
                            });
                            true
                        }
                        "f10" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                model.open_theme_picker();
                            });
                            true
                        }
                        "pageup" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                let panel = model.get_active_panel();
                                let rows = compute_window_rows(panel);
                                let new_index = panel.selected_index.saturating_sub(rows);
                                model.select_entry(new_index);
                            });
                            true
                        }
                        "pagedown" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                let panel = model.get_active_panel();
                                let len = panel.entries.len();
                                let rows = compute_window_rows(panel);
                                let mut new_index = panel.selected_index.saturating_add(rows);
                                if len > 0 && new_index >= len {
                                    new_index = len - 1;
                                }
                                model.select_entry(new_index);
                            });
                            true
                        }
                        _ => false,
                    };

                    if handled {
                        cx.notify();
                        cx.stop_propagation();
                    }
                },
            ))
    }
}

impl FileManagerView {
    #[profiling::function]
    fn render_panel(
        &self,
        panel_side: ActivePanel,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        // pump async directory results to keep UI responsive
        self.model.update(cx, |m: &mut FileSystemModel, cx| {
            let panel = m.panel_mut(panel_side.clone());
            if let Some(rx) = panel.entries_rx.take() {
                match rx.try_recv() {
                    Ok(mut new_entries) => {
                        let start_len = panel.entries.len();
                        panel.entries.append(&mut new_entries);
                        // restore preferred selection if any
                        if let Some(pref) = panel.prefer_select_name.take() {
                            if let Some(idx) = panel.entries.iter().position(|e| e.name == pref) {
                                panel.selected_index = idx;
                                // adjust top to keep in view
                                let window_rows = compute_window_rows(panel);
                                if panel.selected_index < panel.top_index {
                                    panel.top_index = panel.selected_index;
                                } else if panel.selected_index >= panel.top_index + window_rows {
                                    panel.top_index = panel.selected_index + 1 - window_rows;
                                }
                            }
                        }
                        // trigger another frame if we added anything
                        if panel.entries.len() > start_len {
                            cx.notify();
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        panel.entries_rx = Some(rx);
                        cx.notify();
                    }
                    Err(mpsc::TryRecvError::Disconnected) => { /* done */ }
                }
            }
        });

        self.model.update(cx, |m: &mut FileSystemModel, cx2| {
            let p = m.panel_mut(panel_side.clone());
            let window_rows = compute_window_rows(p);
            // only adjust top_index when selection would go out of the visible window
            if p.selected_index < p.top_index {
                p.top_index = p.selected_index;
            } else if p.selected_index >= p.top_index + window_rows {
                p.top_index = p.selected_index + 1 - window_rows;
            }
            // Clamp top_index within valid range considering small lists
            let visible = window_rows.max(1);
            let max_top = p.entries.len().saturating_sub(visible);
            if p.top_index > max_top {
                p.top_index = max_top;
            }
            // Ensure selection remains within range (avoid locking cursor)
            if !p.entries.is_empty() && p.selected_index >= p.entries.len() {
                p.selected_index = p.entries.len() - 1;
            }
            // Keep pumping async RX to ensure view populates without user interaction
            if let Some(rx) = p.entries_rx.take() {
                match rx.try_recv() {
                    Ok(mut new_entries) => {
                        let start_len = p.entries.len();
                        p.entries.append(&mut new_entries);
                        if p.entries.len() > start_len {
                            cx2.notify();
                        }
                        p.entries_rx = Some(rx);
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        p.entries_rx = Some(rx);
                        cx2.notify();
                    }
                    Err(mpsc::TryRecvError::Disconnected) => { /* done */ }
                }
            }
        });
        let model = self.model.read(cx);
        let colors = model.theme.colors();
        let panel = match panel_side {
            ActivePanel::Left => &model.left_panel,
            ActivePanel::Right => &model.right_panel,
        };
        let is_active = model.active_panel == panel_side;
        let target_is_left = matches!(panel_side, ActivePanel::Left);
        let visible_cap: usize = 2000;
        let total_items = panel.entries.len();

        let path_display = match &panel.mode {
            PanelMode::Fs => panel.current_path.to_string_lossy().into_owned(),
            PanelMode::Zip { archive_path, cwd } => {
                if cwd.is_empty() {
                    format!("{}::zip:/", archive_path.to_string_lossy())
                } else {
                    format!("{}::zip:/{}", archive_path.to_string_lossy(), cwd)
                }
            }
        };

        let mut file_list = gpui::div()
            .flex_1()
            .p_2()
            .h_full()
            .w_full()
            .min_w(gpui::px(0.0))
            .children(
                panel
                    .entries
                    .iter()
                    .skip(panel.top_index.min(panel.entries.len().saturating_sub(1)))
                    .take({
                        let start = panel.top_index.min(panel.entries.len().saturating_sub(1));
                        let remain = panel.entries.len().saturating_sub(start);
                        remain.min(visible_cap).max(1)
                    })
                    .enumerate()
                    .map(|(index, entry)| {
                        let real_index = panel.top_index + index;
                        let is_selected = panel.selected_index == real_index;
                        let is_directory = entry.is_dir;

                        gpui::div()
                    .py_1()
                    .px_2()
                    .h(gpui::px(24.0)).min_w(gpui::px(0.0))
                    .w_full()
                    .bg(if is_selected {
                        if is_active {
                            colors.row_bg_selected_active
                        } else {
                            colors.row_bg_selected_inactive
                        }
                    } else {
                        gpui::transparent_black()
                    })
                    .text_color(
                        if is_selected {
                            colors.row_fg_selected
                        } else if is_active {
                            colors.row_fg_active
                        } else {
                            colors.row_fg_inactive
                        }
                    )
                    .font_weight(if is_directory {
                        gpui::FontWeight::BOLD
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .child(format!(
                        "{}{}",
                        if is_directory { "📁 " } else { "📄 " },
                        entry.name
                    ))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(
                            move |this: &mut Self,
                                  event: &gpui::MouseDownEvent,
                                  _window,
                                  cx: &mut gpui::Context<Self>| {
                                if !is_active {
                                    this.model.update(cx, |m: &mut FileSystemModel, _| {
                                        m.active_panel = if target_is_left {
                                            ActivePanel::Left
                                        } else {
                                            ActivePanel::Right
                                        };
                                    });
                                }

                                this.model.update(cx, move |m: &mut FileSystemModel, cx| {
                                    m.select_entry(real_index);
                                    if event.click_count > 1 {
                                        m.open_selected(cx);
                                    }
                                });
                                cx.notify();
                            },
                        ),
                    )
                    }),
            );
        file_list = file_list.on_scroll_wheel(cx.listener(
            move |this: &mut Self,
                  event: &gpui::ScrollWheelEvent,
                  _window,
                  cx: &mut gpui::Context<Self>| {
                let rows: isize = match event.delta {
                    gpui::ScrollDelta::Lines(pt) => {
                        if pt.y > 0.0 {
                            3
                        } else if pt.y < 0.0 {
                            -3
                        } else {
                            0
                        }
                    }
                    gpui::ScrollDelta::Pixels(pt) => {
                        if pt.y > gpui::px(0.0) {
                            3
                        } else if pt.y < gpui::px(0.0) {
                            -3
                        } else {
                            0
                        }
                    }
                };
                this.model.update(cx, |m: &mut FileSystemModel, _| {
                    let p = m.panel_mut(if target_is_left {
                        ActivePanel::Left
                    } else {
                        ActivePanel::Right
                    });
                    let window_rows = compute_window_rows(p);
                    if rows > 0 {
                        p.top_index = p.top_index.saturating_add(rows as usize);
                    } else {
                        p.top_index = p.top_index.saturating_sub((-rows) as usize);
                    }
                    let max_top = p.entries.len().saturating_sub(window_rows.max(1));
                    if p.top_index > max_top {
                        p.top_index = max_top;
                    }
                    // do not change selection here; only adjust top_index via wheel
                    // selection visibility is enforced when moving the cursor or rendering
                });
                cx.notify();
                cx.stop_propagation();
            },
        ));
        file_list.style().overflow = gpui::PointRefinement {
            x: Some(gpui::Overflow::Hidden),
            y: Some(gpui::Overflow::Scroll),
        };
        file_list.style().scrollbar_width = Some(gpui::px(30.0).into());
        if total_items > visible_cap {
            file_list = file_list.child(
                gpui::div()
                    .py_1()
                    .px_2()
                    .w_full()
                    .bg(colors.footer_bg)
                    .text_color(colors.footer_fg)
                    .child(format!("Showing {} of {} items", visible_cap, total_items)),
            );
        }

        gpui::div()
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .min_w(gpui::px(0.0))
            .border_1()
            .border_color(if is_active {
                colors.panel_border_active
            } else {
                colors.panel_border_inactive
            })
            .child(
                // Path header
                gpui::div()
                    .p_2()
                    .bg(colors.header_bg)
                    .text_color(colors.header_fg)
                    .w_full()
                    .w_full()
                    .min_w(gpui::px(0.0))
                    .child(format!(
                        "{}    {}/{}",
                        path_display,
                        if panel.entries.is_empty() {
                            0
                        } else {
                            panel.selected_index + 1
                        },
                        panel.entries.len()
                    )),
            )
            .child({
                if !is_active {
                    let model = self.model.read(cx);

                    if model.preview.is_some() {
                        self.render_preview(cx).into_any_element()
                    } else {
                        file_list
                            .id("list")
                            .track_scroll(&panel.scroll)
                            .into_any_element()
                    }
                } else {
                    file_list
                        .id("list")
                        .track_scroll(&panel.scroll)
                        .into_any_element()
                }
            })
    }

    fn render_preview(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        if let Some(preview) = &model.preview {
            let content = match preview {
                PreviewContent::Text(text) => {
                    let mut area = gpui::div()
                        .p_2()
                        .w_full()
                        .h_full()
                        .text_color(model.theme.colors().preview_text)
                        .child(text.clone());
                    area.style().overflow = gpui::PointRefinement {
                        x: Some(gpui::Overflow::Hidden),
                        y: Some(gpui::Overflow::Scroll),
                    };
                    area.style().scrollbar_width = Some(gpui::px(30.0).into());
                    gpui::div().flex_1().p_2().child(area)
                }
                PreviewContent::Image(path) => {
                    let mut area = gpui::div()
                        .p_2()
                        .w_full()
                        .h_full()
                        .child(gpui::img(path.clone()).w_full().h_full());
                    area.style().overflow = gpui::PointRefinement {
                        x: Some(gpui::Overflow::Hidden),
                        y: Some(gpui::Overflow::Scroll),
                    };
                    area.style().scrollbar_width = Some(gpui::px(30.0).into());
                    gpui::div().flex_1().p_2().child(area)
                }
            };

            gpui::div()
                .flex()
                .flex_col()
                .w_full()
                .h_full()
                .min_w(gpui::px(0.0))
                .bg(model.theme.colors().preview_bg)
                .child(
                    gpui::div()
                        .p_2()
                        .bg(model.theme.colors().preview_header_bg)
                        .text_color(model.theme.colors().preview_header_fg)
                        .child("Preview (F3 to close, Esc to close)"),
                )
                .child(content)
        } else {
            // zero-width placeholder to keep layout simple
            gpui::div().w(gpui::px(0.0)).h_full()
        }
    }

    fn render_theme_picker(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        if !model.theme_picker_open {
            return gpui::div().w(gpui::px(0.0)).h(gpui::px(0.0)).into_any_element();
        }
        let names = model.theme_names();
        let selected = model.theme_picker_selected.unwrap_or(0);
        let colors = model.theme.colors();

        let list = gpui::div()
            .flex()
            .flex_col()
            .w(gpui::px(480.0))
            .max_h(gpui::px(400.0))
            .bg(colors.preview_bg)
            .border_1()
            .border_color(colors.panel_border_active)
            .rounded(gpui::px(6.0))
            .shadow_lg()
            .children(
                names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        let is_sel = i == selected;
                        gpui::div()
                            .px_3()
                            .py_2()
                            .bg(if is_sel { colors.row_bg_selected_active } else { gpui::transparent_black() })
                            .text_color(if is_sel { colors.row_fg_selected } else { colors.row_fg_active })
                            .child(name.clone())
                    }),
            );

        gpui::div()
            .absolute()
            .top(gpui::px(0.0))
            .left(gpui::px(0.0))
            .right(gpui::px(0.0))
            .bottom(gpui::px(0.0))
            .bg(gpui::Hsla::from(gpui::Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.35,
            }))
            .flex()
            .items_center()
            .justify_center()
            .child(list)
            .into_any_element()
    }
}

fn compute_window_rows(panel: &PanelState) -> usize {
    // Measure viewport height via ScrollHandle bounds; if height is zero (not laid out yet),
    // fall back to a conservative default to avoid premature scrolling.
    let bounds = panel.scroll.bounds();
    let height: f32 = bounds.size.height.into();
    let row_px: f32 = 24.0; // row height as set on each entry div

    if height <= 0.0 || row_px <= 0.0 {
        // Fallback: assume a small, safe number of rows to keep selection logic stable
        return 10;
    }

    let rows = (height / row_px).floor() as usize;
    rows.max(1)
}
