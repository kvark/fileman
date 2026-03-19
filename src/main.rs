#![allow(
    // No need for harsh threshold here.
    clippy::too_many_arguments,
)]

use blade_egui as be;
use blade_graphics as bg;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    fs,
    hash::{Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::UNIX_EPOCH,
};

mod image_decode;
mod input;
mod replay_runner;
mod snapshot_render;
mod ui;
#[cfg(feature = "self-update")]
mod update;

use fileman::{app_state, core, theme, workers};
mod replay;

const ROW_HEIGHT: f32 = 24.0;
const SIZE_COL_WIDTH: f32 = 84.0;
const SNAPSHOT_WIDTH: u32 = 800;
const SNAPSHOT_HEIGHT: u32 = 600;
const MAX_IMAGE_TEXTURES: usize = 64;
const MAX_IMAGE_UPLOADS_PER_FRAME: usize = 2;
const MAX_TEXTURE_SIDE: u32 = 1024;
const ARCHIVE_READ_BUFFER: usize = 1024 * 1024;

struct UiCache {
    left_rows: usize,
    right_rows: usize,
    scroll_mode: ScrollMode,
    last_left_selected: usize,
    last_right_selected: usize,
    last_active_panel: core::ActivePanel,
    last_left_dir_token: u64,
    last_right_dir_token: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScrollMode {
    Default,
    ForceActive,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContainerLoadMode {
    UseCache,
    ForceReload,
}

impl UiCache {
    fn update_scroll_mode(&mut self, app: &app_state::AppState) {
        let left_selected = app.left_panel.browser().selected_index;
        let right_selected = app.right_panel.browser().selected_index;
        let active = app.active_panel;
        let left_dir = app.left_panel.browser().dir_token;
        let right_dir = app.right_panel.browser().dir_token;
        // Don't trigger ForceActive on Tab alone — ensure_visible handles
        // bringing the selection into view without unnecessary re-centering.
        let selection_changed = left_selected != self.last_left_selected
            || right_selected != self.last_right_selected
            || left_dir != self.last_left_dir_token
            || right_dir != self.last_right_dir_token;
        self.scroll_mode = if selection_changed {
            ScrollMode::ForceActive
        } else {
            ScrollMode::Default
        };
        self.last_left_selected = left_selected;
        self.last_right_selected = right_selected;
        self.last_active_panel = active;
        self.last_left_dir_token = left_dir;
        self.last_right_dir_token = right_dir;
    }
}

struct ImageRequest {
    key: String,
    source: ImageSource,
}

struct ImageResult {
    key: String,
    image: image_decode::DecodedImage,
    meta: image_decode::ImageMeta,
    /// When true, a higher-quality version is still being decoded.
    refining: bool,
}

enum ImageResponse {
    Ok(ImageResult),
    Err { key: String, message: String },
}

enum ImageSource {
    Fs(PathBuf),
    Container {
        kind: core::ContainerKind,
        archive_path: PathBuf,
        inner_path: String,
    },
}

struct HighlightRequest {
    key: String,
    text: String,
    ext: Option<String>,
    theme_kind: theme::ThemeKind,
}

struct HighlightResult {
    key: String,
    job: egui::text::LayoutJob,
}

struct AnimationData {
    frames: Vec<egui::ColorImage>,
    delays: Vec<u32>,
    total_duration_ms: u64,
}

#[derive(Default)]
struct ImageCache {
    textures: HashMap<String, egui::TextureHandle>,
    animations: HashMap<String, AnimationData>,
    meta: HashMap<String, image_decode::ImageMeta>,
    failures: HashMap<String, String>,
    pending: HashSet<String>,
    /// Keys where a fast preview is shown but full decode is still in progress,
    /// mapped to when the current tier was first displayed.
    refining: HashMap<String, std::time::Instant>,
    order: VecDeque<String>,
}

/// Minimum time a progressive preview tier is shown before being replaced.
const MIN_REFINING_DISPLAY: std::time::Duration = std::time::Duration::from_millis(150);

fn touch_image(cache: &mut ImageCache, key: &str) {
    if let Some(pos) = cache.order.iter().position(|p| p == key) {
        cache.order.remove(pos);
        cache.order.push_back(key.to_string());
    }
}

fn color32(c: theme::Color) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (c.r.clamp(0.0, 1.0) * 255.0) as u8,
        (c.g.clamp(0.0, 1.0) * 255.0) as u8,
        (c.b.clamp(0.0, 1.0) * 255.0) as u8,
        (c.a.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

fn blend_color(base: theme::Color, tint: theme::Color, t: f32) -> theme::Color {
    let t = t.clamp(0.0, 1.0);
    theme::Color::rgba(
        base.r + (tint.r - base.r) * t,
        base.g + (tint.g - base.g) * t,
        base.b + (tint.b - base.b) * t,
        base.a,
    )
}

fn fade_color(color: theme::Color, factor: f32) -> theme::Color {
    theme::Color::rgba(
        color.r,
        color.g,
        color.b,
        (color.a * factor).clamp(0.0, 1.0),
    )
}

fn cursor_row_col(text: &str, cursor: usize) -> (usize, usize) {
    let mut row = 1usize;
    let mut col = 1usize;
    for (idx, ch) in text.chars().enumerate() {
        if idx >= cursor {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (row, col)
}

static SYNTAX_SET: once_cell::sync::Lazy<syntect::parsing::SyntaxSet> =
    once_cell::sync::Lazy::new(syntect::parsing::SyntaxSet::load_defaults_newlines);
static THEME_SET: once_cell::sync::Lazy<syntect::highlighting::ThemeSet> =
    once_cell::sync::Lazy::new(syntect::highlighting::ThemeSet::load_defaults);

fn apply_theme(ctx: &egui::Context, colors: &theme::ThemeColors) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::Vec2::new(8.0, 6.0);
    style.spacing.window_margin = egui::Margin::same(8);
    style.visuals.window_fill = color32(colors.preview_bg);
    style.visuals.panel_fill = color32(colors.preview_bg);
    style.visuals.extreme_bg_color = color32(colors.header_bg);
    style.visuals.window_stroke.color = color32(colors.panel_border_inactive);
    style.visuals.window_corner_radius = egui::CornerRadius::same(6);
    style.visuals.menu_corner_radius = egui::CornerRadius::same(6);
    style.visuals.faint_bg_color = color32(colors.divider);
    style.visuals.code_bg_color = color32(colors.footer_bg);
    style.visuals.selection.bg_fill = color32(colors.row_bg_selected_active);
    style.visuals.selection.stroke.color = color32(colors.row_fg_selected);
    style.visuals.widgets.inactive.bg_fill = color32(colors.preview_bg);
    style.visuals.widgets.inactive.fg_stroke.color = color32(colors.row_fg_inactive);
    style.visuals.widgets.active.bg_fill = color32(colors.row_bg_selected_active);
    style.visuals.widgets.active.fg_stroke.color = color32(colors.row_fg_selected);
    style.visuals.widgets.hovered.bg_fill = color32(colors.row_bg_selected_inactive);
    style.visuals.widgets.hovered.fg_stroke.color = color32(colors.row_fg_active);
    style.visuals.hyperlink_color = color32(colors.panel_border_active);
    style.visuals.override_text_color = Some(color32(colors.row_fg_active));
    ctx.set_style(style);
}

fn app_icon() -> Option<winit::window::Icon> {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let idx = ((y * size + x) * 4) as usize;
            let (r, g, b) = if x == 0 || y == 0 || x == size - 1 || y == size - 1 {
                (40, 60, 90)
            } else if x == size / 2 {
                (70, 90, 120)
            } else if x < size / 2 {
                (35, 45, 65)
            } else {
                (28, 38, 55)
            };
            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = 255;
        }
    }
    winit::window::Icon::from_rgba(rgba, size, size).ok()
}

fn pick_theme(theme_kind: theme::ThemeKind) -> &'static syntect::highlighting::Theme {
    let themes = &THEME_SET.themes;
    let key = match theme_kind {
        theme::ThemeKind::Dark => "base16-ocean.dark",
        theme::ThemeKind::Light => "InspiredGitHub",
    };
    themes
        .get(key)
        .or_else(|| themes.values().next())
        .expect("syntect theme")
}

fn highlight_text_job(
    text: &str,
    extension: Option<&str>,
    theme_kind: theme::ThemeKind,
) -> egui::text::LayoutJob {
    let ext = extension.map(|ext| ext.to_ascii_lowercase());
    if ext.as_deref() == Some("toml") {
        return fileman::syntax::toml::highlight_toml_job(text, theme_kind);
    }
    if ext.as_deref() == Some("nix") {
        return fileman::syntax::nix::highlight_nix_job(text, theme_kind);
    }
    if ext.as_deref() == Some("ron") {
        return fileman::syntax::ron::highlight_ron_job(text, theme_kind);
    }
    let by_name_ci = |name: &str| {
        let needle = name.to_ascii_lowercase();
        SYNTAX_SET
            .syntaxes()
            .iter()
            .find(|s| s.name.to_ascii_lowercase().contains(&needle))
    };
    let syntax = ext
        .as_deref()
        .and_then(|ext| SYNTAX_SET.find_syntax_by_extension(ext))
        .or_else(|| {
            ext.as_deref().and_then(|ext| match ext {
                "toml" => by_name_ci("toml"),
                "yml" | "yaml" => by_name_ci("yaml"),
                "rs" => SYNTAX_SET.find_syntax_by_name("Rust"),
                "md" => SYNTAX_SET.find_syntax_by_name("Markdown"),
                "json" | "gltf" => SYNTAX_SET.find_syntax_by_name("JSON"),
                "js" => SYNTAX_SET.find_syntax_by_name("JavaScript"),
                "ts" => SYNTAX_SET.find_syntax_by_name("TypeScript"),
                "css" => SYNTAX_SET.find_syntax_by_name("CSS"),
                "html" => SYNTAX_SET.find_syntax_by_name("HTML"),
                _ => None,
            })
        })
        .or_else(|| SYNTAX_SET.find_syntax_by_first_line(text))
        .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());
    let mut highlighter = syntect::easy::HighlightLines::new(syntax, pick_theme(theme_kind));
    let mut job = egui::text::LayoutJob::default();
    for line in syntect::util::LinesWithEndings::from(text) {
        let ranges = highlighter
            .highlight_line(line, &SYNTAX_SET)
            .unwrap_or_else(|_| vec![(syntect::highlighting::Style::default(), line)]);
        for (style, piece) in ranges {
            let color = egui::Color32::from_rgba_unmultiplied(
                style.foreground.r,
                style.foreground.g,
                style.foreground.b,
                style.foreground.a,
            );
            let format = egui::TextFormat {
                font_id: egui::FontId::monospace(13.0),
                color,
                ..Default::default()
            };
            job.append(piece, 0.0, format);
        }
    }
    job
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn surface_error_help() -> &'static str {
    "Blade-graphics could not find a supported GPU backend.\n\
Try one of:\n\
  - Install Vulkan drivers for your GPU and re-run.\n\
  - Build with GLES fallback: RUSTFLAGS=\"--cfg gles\" cargo run\n\
On Linux in CI or headless environments, Vulkan is often unavailable."
}

fn panel_path_display(panel: &app_state::PanelState) -> String {
    let browser = panel.browser();
    let app_state::BrowserState {
        browser_mode: ref mode,
        ..
    } = *browser;
    match mode {
        core::BrowserMode::Fs => browser.current_path.to_string_lossy().into_owned(),
        core::BrowserMode::Container {
            kind,
            archive_path,
            cwd,
            root,
        } => {
            let display_cwd = if let Some(root) = root.as_ref()
                && !root.is_empty()
                && cwd.starts_with(root)
            {
                cwd[root.len()..].trim_start_matches('/').to_string()
            } else {
                cwd.clone()
            };
            core::container_display_path(*kind, archive_path, &display_cwd)
        }
        core::BrowserMode::Search {
            root,
            query,
            mode,
            case,
        } => {
            let mode_label = match mode {
                core::SearchMode::Name => "name",
                core::SearchMode::Content => "content",
            };
            let case_label = match case {
                core::SearchCase::Sensitive => "Aa",
                core::SearchCase::Insensitive => "aA",
            };
            format!(
                "Search ({mode_label}/{case_label}): \"{query}\" in {}",
                root.to_string_lossy()
            )
        }
    }
}

fn cmp_option_u64(a: Option<u64>, b: Option<u64>, descending: bool) -> Ordering {
    match (a, b) {
        (Some(av), Some(bv)) => {
            if descending {
                bv.cmp(&av)
            } else {
                av.cmp(&bv)
            }
        }
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
    }
}

fn sort_entries(entries: &mut Vec<core::DirEntry>, mode: core::SortMode, descending: bool) {
    if mode == core::SortMode::Raw {
        return;
    }

    let parent_index = entries.iter().position(|entry| entry.name == "..");
    let parent = parent_index.map(|idx| entries.remove(idx));

    entries.sort_by(|a, b| {
        if a.is_dir != b.is_dir {
            return b.is_dir.cmp(&a.is_dir);
        }
        let mut ord = match mode {
            core::SortMode::Name => {
                if descending {
                    b.name.cmp(&a.name)
                } else {
                    a.name.cmp(&b.name)
                }
            }
            core::SortMode::Date => cmp_option_u64(a.modified, b.modified, descending),
            core::SortMode::Size => {
                if a.is_dir && b.is_dir {
                    if descending {
                        b.name.cmp(&a.name)
                    } else {
                        a.name.cmp(&b.name)
                    }
                } else {
                    cmp_option_u64(a.size, b.size, descending)
                }
            }
            core::SortMode::Raw => Ordering::Equal,
        };
        if ord == Ordering::Equal {
            ord = if descending {
                b.name.cmp(&a.name)
            } else {
                a.name.cmp(&b.name)
            };
        }
        ord
    });

    if let Some(parent) = parent {
        entries.insert(0, parent);
    }
}

fn resort_browser_entries(browser: &mut app_state::BrowserState) {
    let selected_name = browser
        .entries
        .get(browser.selected_index)
        .map(|entry| entry.name.clone());
    sort_entries(&mut browser.entries, browser.sort_mode, browser.sort_desc);
    if let Some(name) = selected_name
        && let Some(idx) = browser.entries.iter().position(|entry| entry.name == name)
    {
        browser.selected_index = idx;
    }
    if browser.selected_index < browser.top_index {
        browser.top_index = browser.selected_index;
    }
}

fn sort_mode_label(mode: core::SortMode) -> &'static str {
    match mode {
        core::SortMode::Name => "Name",
        core::SortMode::Date => "Date",
        core::SortMode::Size => "Size",
        core::SortMode::Raw => "Raw",
    }
}

fn rebuild_search_entries(browser: &mut app_state::BrowserState, results: &[core::SearchResult]) {
    let app_state::BrowserState {
        browser_mode: ref mode,
        ..
    } = *browser;
    browser.entries = results
        .iter()
        .map(|result| {
            let display_name = match mode {
                core::BrowserMode::Search { root, .. } => result
                    .path
                    .strip_prefix(root)
                    .ok()
                    .and_then(|p| p.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        result
                            .path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("<unknown>")
                            .to_string()
                    }),
                _ => result
                    .path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("<unknown>")
                    .to_string(),
            };
            core::DirEntry {
                name: display_name,
                is_dir: result.is_dir,
                is_symlink: false,
                link_target: None,
                location: core::EntryLocation::Fs(result.path.clone()),
                size: result.size,
                modified: result.modified,
            }
        })
        .collect();
}

fn hexdump_job(
    bytes: &[u8],
    width: usize,
    colors: &theme::ThemeColors,
    ui: &egui::Ui,
) -> egui::text::LayoutJob {
    let width = width.clamp(4, 32);
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let offset_color = color32(colors.row_fg_inactive);
    let hex_color = color32(colors.row_fg_active);
    let ascii_color = color32(colors.row_fg_inactive);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    job.wrap.break_anywhere = false;

    let offset_format = egui::TextFormat {
        font_id: font_id.clone(),
        color: offset_color,
        ..Default::default()
    };
    let hex_format = egui::TextFormat {
        font_id: font_id.clone(),
        color: hex_color,
        ..Default::default()
    };
    let ascii_format = egui::TextFormat {
        font_id,
        color: ascii_color,
        ..Default::default()
    };

    let mut offset = 0usize;
    for chunk in bytes.chunks(width) {
        let mut line = String::new();
        line.push_str(&format!("{:08x}: ", offset));
        job.append(&line, 0.0, offset_format.clone());

        let mut hex = String::new();
        for i in 0..width {
            if i < chunk.len() {
                hex.push_str(&format!("{:02x} ", chunk[i]));
            } else {
                hex.push_str("   ");
            }
            if i == (width / 2).saturating_sub(1) {
                hex.push(' ');
            }
        }
        job.append(&hex, 0.0, hex_format.clone());

        let mut ascii = String::new();
        ascii.push(' ');
        for &b in chunk {
            let ch = if (0x20..=0x7e).contains(&b) {
                b as char
            } else {
                '.'
            };
            ascii.push(ch);
        }
        ascii.push('\n');
        job.append(&ascii, 0.0, ascii_format.clone());

        offset += width;
    }

    job
}

fn apply_dir_batch(browser: &mut app_state::BrowserState, batch: core::DirBatch) {
    let prior_selection = browser
        .entries
        .get(browser.selected_index)
        .map(|e| e.name.clone());

    match batch {
        core::DirBatch::Loading => {
            browser.loading = true;
            browser.loading_progress = None;
            return;
        }
        core::DirBatch::ContainerRoot(root) => {
            browser.container_root = root;
            if let core::BrowserMode::Container {
                kind,
                archive_path,
                cwd,
                ..
            } = &browser.browser_mode
            {
                browser.browser_mode = core::BrowserMode::Container {
                    kind: *kind,
                    archive_path: archive_path.clone(),
                    cwd: cwd.clone(),
                    root: browser.container_root.clone(),
                };
            }
            return;
        }
        core::DirBatch::Error(message) => {
            let mut entries = Vec::new();
            if let Some(parent) = browser.current_path.parent() {
                entries.push(core::DirEntry {
                    name: "..".to_string(),
                    is_dir: true,
                    is_symlink: false,
                    link_target: None,
                    location: core::EntryLocation::Fs(parent.to_path_buf()),
                    size: None,
                    modified: None,
                });
            }
            entries.push(core::DirEntry {
                name: message,
                is_dir: false,
                is_symlink: false,
                link_target: None,
                location: core::EntryLocation::Fs(browser.current_path.clone()),
                size: None,
                modified: None,
            });
            browser.entries = entries;
            browser.selected_index = 0;
            browser.top_index = 0;
            browser.loading = false;
            browser.loading_progress = None;
            return;
        }
        core::DirBatch::Progress { loaded, total } => {
            browser.loading_progress = Some((loaded, total));
            browser.loading = total.map(|t| loaded < t).unwrap_or(true);
            return;
        }
        core::DirBatch::Append(mut new_entries) => {
            browser.entries.append(&mut new_entries);
            browser.loading = false;
        }
        core::DirBatch::Replace(new_entries) => {
            browser.entries = new_entries;
            browser.selected_index = 0;
            browser.loading = false;
        }
    }

    let restore_name = browser.prefer_select_name.clone().or(prior_selection);
    sort_entries(&mut browser.entries, browser.sort_mode, browser.sort_desc);
    if let Some(ref pref) = restore_name
        && let Some(idx) = browser.entries.iter().position(|e| e.name == *pref)
    {
        browser.selected_index = idx;
        // Only consume the preference once the entry is found
        if browser.prefer_select_name.as_deref() == Some(pref.as_str()) {
            browser.prefer_select_name = None;
        }
    }
    if browser.selected_index < browser.top_index {
        browser.top_index = browser.selected_index;
    }
}

fn pump_async(app: &mut app_state::AppState) -> bool {
    let mut changed = false;
    for side in [core::ActivePanel::Left, core::ActivePanel::Right] {
        let panel = app.panel_mut(side);
        let browser = panel.browser_mut();
        if let Some(rx) = browser.entries_rx.take() {
            let mut handled = 0usize;
            while handled < 8 {
                match rx.try_recv() {
                    Ok(batch) => {
                        apply_dir_batch(browser, batch);
                        handled += 1;
                        changed = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        browser.entries_rx = Some(rx);
                        break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
            }
        }
    }

    // Poll shared archive indexes for incremental updates
    for side in [core::ActivePanel::Left, core::ActivePanel::Right] {
        let watching = app.panel(side).browser().watching_archive.clone();
        if let Some(ref archive_path) = watching
            && let Some(shared) = app.archive_index.get(archive_path).cloned()
        {
            let (entry_count, complete, root) = {
                let idx = shared.lock().unwrap();
                (idx.entries.len(), idx.complete, idx.root.clone())
            };
            let panel = app.panel_mut(side);
            let browser = panel.browser_mut();
            let last_seen = browser.index_last_seen;
            if entry_count > last_seen || complete {
                if let core::BrowserMode::Container {
                    kind,
                    ref archive_path,
                    ref cwd,
                    ..
                } = browser.browser_mode
                {
                    let cwd = cwd.clone();
                    let archive_path = archive_path.clone();
                    let idx = shared.lock().unwrap();
                    let mut listing = build_listing_from_index(&idx, &archive_path, kind, &cwd);
                    drop(idx);
                    sort_entries(&mut listing, browser.sort_mode, browser.sort_desc);
                    // Only replace entries if the filtered listing actually changed.
                    // The raw index grows with entries from all directories, but
                    // the listing for this cwd may be unchanged. Skipping avoids
                    // disrupting the user's cursor position.
                    let listing_changed = listing.len() != browser.entries.len();
                    if listing_changed {
                        let prior_name = browser
                            .entries
                            .get(browser.selected_index)
                            .map(|e| e.name.clone());
                        let restore_name = browser.prefer_select_name.take().or(prior_name);
                        browser.entries = listing;
                        if let Some(ref pref) = restore_name
                            && let Some(pos) = browser.entries.iter().position(|e| e.name == *pref)
                        {
                            browser.selected_index = pos;
                        }
                        changed = true;
                    }
                    browser.index_last_seen = entry_count;
                    browser.loading_progress = Some((entry_count, None));
                    if let Some(ref root) = root {
                        browser.container_root = Some(root.clone());
                        browser.browser_mode = core::BrowserMode::Container {
                            kind,
                            archive_path: archive_path.clone(),
                            cwd,
                            root: Some(root.clone()),
                        };
                    }
                }
                if complete {
                    let browser = app.panel_mut(side).browser_mut();
                    browser.watching_archive = None;
                    browser.loading = false;
                    browser.loading_progress = None;
                    changed = true;
                }
            }
        }
    }

    if let Ok((id, content)) = app.preview_rx.try_recv()
        && let Some(preview) = app.preview_panel_mut()
        && id == preview.request_id
    {
        match content {
            core::PreviewContent::TextChunk { text, .. } => match preview.content {
                Some(core::PreviewContent::Text(ref mut existing)) => {
                    existing.push_str(&text);
                }
                _ => preview.content = Some(core::PreviewContent::Text(text)),
            },
            core::PreviewContent::BinaryChunk { data, .. } => match preview.content {
                Some(core::PreviewContent::Binary(ref mut existing)) => {
                    existing.extend_from_slice(&data);
                }
                _ => preview.content = Some(core::PreviewContent::Binary(data)),
            },
            other => preview.content = Some(other),
        }
        preview.loading_since = None;
        changed = true;
    }
    if let Some(preview) = app.preview_panel()
        && let Some(started) = preview.loading_since
        && started.elapsed() > std::time::Duration::from_millis(300)
    {
        changed = true;
    }

    while let Ok((path, size)) = app.dir_size_rx.try_recv() {
        app.dir_size_pending.remove(&path);
        app.dir_sizes.insert(path.clone(), size);
        for side in [core::ActivePanel::Left, core::ActivePanel::Right] {
            let panel = app.panel_mut(side);
            let browser = panel.browser_mut();
            let mut updated = false;
            for entry in &mut browser.entries {
                if entry.is_dir
                    && let core::EntryLocation::Fs(p) = &entry.location
                    && *p == path
                {
                    entry.size = Some(size);
                    updated = true;
                }
            }
            if updated && browser.sort_mode == core::SortMode::Size {
                resort_browser_entries(browser);
            }
        }
        changed = true;
    }

    while let Ok(result) = app.edit_rx.try_recv() {
        if let Some(edit) = app.edit_panel_mut()
            && result.id == edit.request_id
        {
            edit.loading = false;
            edit.text = result.text;
            edit.highlight_hash = hash_text(&edit.text);
            edit.highlight_wrap_width = 0.0;
            edit.highlight_key = Some(format!("edit:{}", result.path.to_string_lossy()));
            edit.highlight_dirty_at = None;
            edit.dirty = false;
            edit.confirm_discard = false;
            changed = true;
        }
    }

    while let Ok(event) = app.search_rx.try_recv() {
        match event {
            core::SearchEvent::Match { id, result } => {
                if id == app.search_request_id {
                    app.search_results.push(result);
                    let result = app.search_results.last().unwrap().clone();
                    let progress_for_panel = match app.search_status {
                        app_state::SearchStatus::Running(mut progress) => {
                            progress.matched = progress.matched.saturating_add(1);
                            app.search_status = app_state::SearchStatus::Running(progress);
                            Some((progress.matched, None))
                        }
                        app_state::SearchStatus::Done(mut progress) => {
                            progress.matched = progress.matched.saturating_add(1);
                            app.search_status = app_state::SearchStatus::Done(progress);
                            Some((progress.matched, None))
                        }
                        app_state::SearchStatus::Idle => None,
                    };
                    let panel = app.get_active_panel_mut();
                    let browser = panel.browser_mut();
                    let app_state::BrowserState {
                        browser_mode: ref mode,
                        ..
                    } = *browser;
                    let display_name = match mode {
                        core::BrowserMode::Search { root, .. } => result
                            .path
                            .strip_prefix(root)
                            .ok()
                            .and_then(|p| p.to_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| {
                                result
                                    .path
                                    .file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("<unknown>")
                                    .to_string()
                            }),
                        _ => result
                            .path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("<unknown>")
                            .to_string(),
                    };
                    browser.entries.push(core::DirEntry {
                        name: display_name,
                        is_dir: result.is_dir,
                        is_symlink: false,
                        link_target: None,
                        location: core::EntryLocation::Fs(result.path),
                        size: result.size,
                        modified: result.modified,
                    });
                    resort_browser_entries(browser);
                    if let Some(progress) = progress_for_panel {
                        browser.loading_progress = Some(progress);
                    }
                    changed = true;
                }
            }
            core::SearchEvent::Progress { id, progress } => {
                if id == app.search_request_id {
                    app.search_status = app_state::SearchStatus::Running(progress);
                    let panel = app.get_active_panel_mut();
                    panel.browser_mut().loading_progress =
                        Some((progress.matched, Some(progress.scanned)));
                    changed = true;
                }
            }
            core::SearchEvent::Done { id, progress } => {
                if id == app.search_request_id {
                    app.search_status = app_state::SearchStatus::Done(progress);
                    let panel = app.get_active_panel_mut();
                    panel.browser_mut().loading = false;
                    panel.browser_mut().loading_progress =
                        Some((progress.matched, Some(progress.scanned)));
                    changed = true;
                }
            }
            core::SearchEvent::Error { id, message } => {
                if id == app.search_request_id {
                    eprintln!("Search error: {message}");
                    app.search_status = app_state::SearchStatus::Done(core::SearchProgress {
                        scanned: 0,
                        matched: 0,
                    });
                    let panel = app.get_active_panel_mut();
                    panel.browser_mut().loading = false;
                    changed = true;
                }
            }
        }
    }

    app.poll_update_status();

    changed
}

fn load_fs_directory_async(
    app: &mut app_state::AppState,
    path: PathBuf,
    target_panel: core::ActivePanel,
    prefer_name: Option<String>,
) {
    let same_dir = app.panel(target_panel).browser().current_path == path;
    let mut initial: Vec<core::DirEntry> = Vec::new();
    let mut has_parent_entry = false;
    if path.parent().is_some() {
        initial.push(core::DirEntry {
            name: "..".to_string(),
            is_dir: true,
            is_symlink: false,
            link_target: None,
            location: core::EntryLocation::Fs(path.parent().unwrap().to_path_buf()),
            size: None,
            modified: None,
        });
        has_parent_entry = true;
    }

    app.stash_container_cache(target_panel);
    let (tx, rx) = mpsc::channel::<core::DirBatch>();
    let path_clone = path.clone();
    let wake = app.wake.clone();
    let dir_sizes_snapshot = app.dir_sizes.clone();
    let dir_sizes_fallback = app.dir_sizes.clone();

    if let Ok(mut rd) = fs::read_dir(&path) {
        let mut snapshot: Vec<core::DirEntry> = Vec::with_capacity(128);
        for _ in 0..128 {
            match rd.next() {
                Some(Ok(entry)) => {
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    let ft = entry.file_type().ok();
                    let is_symlink = ft.as_ref().map(|f| f.is_symlink()).unwrap_or(false);
                    let metadata = if is_symlink {
                        fs::metadata(entry.path()).ok()
                    } else {
                        entry.metadata().ok()
                    };
                    let is_dir = if is_symlink {
                        metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false)
                    } else {
                        ft.map(|f| f.is_dir()).unwrap_or(false)
                    };
                    let size = if is_dir {
                        dir_sizes_snapshot.get(&entry.path()).copied()
                    } else {
                        metadata.as_ref().map(|m| m.len())
                    };
                    let modified = metadata
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    let link_target = if is_symlink {
                        fs::read_link(entry.path())
                            .ok()
                            .map(|t| t.to_string_lossy().into_owned())
                    } else {
                        None
                    };
                    snapshot.push(core::DirEntry {
                        name: file_name,
                        is_dir,
                        is_symlink,
                        link_target,
                        location: core::EntryLocation::Fs(entry.path()),
                        size,
                        modified,
                    });
                }
                Some(Err(_)) | None => break,
            }
        }
        // For same-dir reloads, skip the initial Append to avoid transient
        // duplicates (old entries + appended snapshot) that corrupt ScrollArea state.
        if !snapshot.is_empty() && !same_dir {
            let _ = tx.send(core::DirBatch::Append(snapshot.clone()));
        }
        thread::spawn(move || {
            let chunk = 500usize;
            let mut all: Vec<core::DirEntry> = snapshot;
            for entry in rd.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();
                if let Ok(file_type) = entry.file_type() {
                    let is_symlink = file_type.is_symlink();
                    let metadata = if is_symlink {
                        fs::metadata(entry.path()).ok()
                    } else {
                        entry.metadata().ok()
                    };
                    let is_dir = if is_symlink {
                        metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false)
                    } else {
                        file_type.is_dir()
                    };
                    let size = if is_dir {
                        dir_sizes_snapshot.get(&entry.path()).copied()
                    } else {
                        metadata.as_ref().map(|m| m.len())
                    };
                    let modified = metadata
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    let link_target = if is_symlink {
                        fs::read_link(entry.path())
                            .ok()
                            .map(|t| t.to_string_lossy().into_owned())
                    } else {
                        None
                    };
                    all.push(core::DirEntry {
                        name: file_name,
                        is_dir,
                        is_symlink,
                        link_target,
                        location: core::EntryLocation::Fs(entry.path()),
                        size,
                        modified,
                    });
                }
            }
            let mut sorted: Vec<core::DirEntry> = Vec::new();
            if let Some(parent) = path_clone.parent() {
                sorted.push(core::DirEntry {
                    name: "..".to_string(),
                    is_dir: true,
                    is_symlink: false,
                    link_target: None,
                    location: core::EntryLocation::Fs(parent.to_path_buf()),
                    size: None,
                    modified: None,
                });
            }
            sorted.extend(all);

            if sorted.is_empty() {
                return;
            }
            let mut start = 0usize;
            while start < sorted.len() {
                let end = (start + chunk).min(sorted.len());
                let batch = sorted[start..end].to_vec();
                if start == 0 {
                    let _ = tx.send(core::DirBatch::Replace(batch));
                } else {
                    let _ = tx.send(core::DirBatch::Append(batch));
                }
                start = end;
            }
        });
    } else {
        thread::spawn(move || {
            let chunk = 500usize;
            let mut all: Vec<core::DirEntry> = Vec::new();
            match fs::read_dir(&path_clone) {
                Err(e) => {
                    let _ = tx.send(core::DirBatch::Error(format!("Cannot read directory: {e}")));
                    if let Some(ref wake) = wake {
                        wake();
                    }
                    return;
                }
                Ok(read_dir) => {
                    for entry in read_dir.flatten() {
                        let file_name = entry.file_name().to_string_lossy().to_string();
                        if let Ok(file_type) = entry.file_type() {
                            let is_symlink = file_type.is_symlink();
                            let metadata = if is_symlink {
                                fs::metadata(entry.path()).ok()
                            } else {
                                entry.metadata().ok()
                            };
                            let is_dir = if is_symlink {
                                metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false)
                            } else {
                                file_type.is_dir()
                            };
                            let size = if is_dir {
                                dir_sizes_fallback.get(&entry.path()).copied()
                            } else {
                                metadata.as_ref().map(|m| m.len())
                            };
                            let modified = metadata
                                .and_then(|m| m.modified().ok())
                                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                                .map(|d| d.as_secs());
                            let link_target = if is_symlink {
                                fs::read_link(entry.path())
                                    .ok()
                                    .map(|t| t.to_string_lossy().into_owned())
                            } else {
                                None
                            };
                            all.push(core::DirEntry {
                                name: file_name,
                                is_dir,
                                is_symlink,
                                link_target,
                                location: core::EntryLocation::Fs(entry.path()),
                                size,
                                modified,
                            });
                        }
                    }
                }
            }
            let mut sorted: Vec<core::DirEntry> = Vec::new();
            if let Some(parent) = path_clone.parent() {
                sorted.push(core::DirEntry {
                    name: "..".to_string(),
                    is_dir: true,
                    is_symlink: false,
                    link_target: None,
                    location: core::EntryLocation::Fs(parent.to_path_buf()),
                    size: None,
                    modified: None,
                });
            }
            sorted.extend(all);
            if sorted.is_empty() {
                return;
            }
            let mut start = 0usize;
            while start < sorted.len() {
                let end = (start + chunk).min(sorted.len());
                let batch = sorted[start..end].to_vec();
                if start == 0 {
                    let _ = tx.send(core::DirBatch::Replace(batch));
                } else {
                    let _ = tx.send(core::DirBatch::Append(batch));
                }
                if let Some(ref wake) = wake {
                    wake();
                }
                start = end;
            }
        });
    }

    let remembered = prefer_name
        .clone()
        .or_else(|| app.fs_last_selected_name.get(&path).cloned());
    let panel_state = app.panel_mut(target_panel);
    let browser = panel_state.browser_mut();
    let initial_loading = initial.is_empty() || has_parent_entry;
    if !same_dir {
        browser.marked.clear();
        browser.top_index = 0;
        browser.entries = initial;
        browser.selected_index = 0;
    }
    browser.current_path = path.clone();
    browser.browser_mode = core::BrowserMode::Fs;
    browser.inline_rename = None;
    browser.dir_token = browser.dir_token.wrapping_add(1);
    browser.entries_rx = Some(rx);
    browser.prefer_select_name = remembered;
    browser.loading = initial_loading;
    browser.loading_progress = None;
    browser.watching_archive = None;
    browser.index_last_seen = 0;
}

fn build_listing_from_index(
    index: &app_state::ArchiveFullIndex,
    archive_path: &Path,
    kind: core::ContainerKind,
    cwd: &str,
) -> Vec<core::DirEntry> {
    let root = index.root.as_deref();
    let mut prefix = String::new();
    if let Some(r) = root {
        prefix.push_str(r.trim_end_matches('/'));
        prefix.push('/');
    }
    if !cwd.is_empty() {
        prefix.push_str(cwd.trim_end_matches('/'));
        prefix.push('/');
    }

    let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut dir_list: Vec<String> = Vec::new();
    let mut file_list: Vec<(String, Option<u64>)> = Vec::new();
    let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (name, is_dir, size) in &index.entries {
        if !name.starts_with(&prefix) {
            continue;
        }
        let rem = &name[prefix.len()..];
        if rem.is_empty() {
            continue;
        }
        if let Some(slash) = rem.find('/') {
            let dir = &rem[..slash];
            if seen_dirs.insert(dir.to_string()) {
                dir_list.push(dir.to_string());
            }
        } else if *is_dir {
            if seen_dirs.insert(rem.to_string()) {
                dir_list.push(rem.to_string());
            }
        } else if !seen_dirs.contains(rem) && seen_files.insert(rem.to_string()) {
            file_list.push((rem.to_string(), *size));
        }
    }

    let mut entries = Vec::new();

    // ".." entry
    if !cwd.is_empty() {
        let mut parent = cwd
            .trim_end_matches('/')
            .rsplit_once('/')
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
        if let Some(r) = root
            && parent == *r
        {
            parent.clear();
        }
        entries.push(core::DirEntry {
            name: "..".into(),
            is_dir: true,
            is_symlink: false,
            link_target: None,
            location: core::EntryLocation::Container {
                kind,
                archive_path: archive_path.to_path_buf(),
                inner_path: parent,
            },
            size: None,
            modified: None,
        });
    } else {
        let parent = archive_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        entries.push(core::DirEntry {
            name: "..".into(),
            is_dir: true,
            is_symlink: false,
            link_target: None,
            location: core::EntryLocation::Fs(parent),
            size: None,
            modified: None,
        });
    }

    for d in dir_list {
        let inner = if let Some(r) = root {
            if cwd.is_empty() {
                format!("{}/{}", r.trim_end_matches('/'), d)
            } else {
                format!(
                    "{}/{}/{}",
                    r.trim_end_matches('/'),
                    cwd.trim_end_matches('/'),
                    d
                )
            }
        } else if cwd.is_empty() {
            d.clone()
        } else {
            format!("{}/{}", cwd.trim_end_matches('/'), d)
        };
        entries.push(core::DirEntry {
            name: d,
            is_dir: true,
            is_symlink: false,
            link_target: None,
            location: core::EntryLocation::Container {
                kind,
                archive_path: archive_path.to_path_buf(),
                inner_path: inner,
            },
            size: None,
            modified: None,
        });
    }

    for (f, size) in file_list {
        let inner = if let Some(r) = root {
            if cwd.is_empty() {
                format!("{}/{}", r.trim_end_matches('/'), f)
            } else {
                format!(
                    "{}/{}/{}",
                    r.trim_end_matches('/'),
                    cwd.trim_end_matches('/'),
                    f
                )
            }
        } else if cwd.is_empty() {
            f.clone()
        } else {
            format!("{}/{}", cwd.trim_end_matches('/'), f)
        };
        entries.push(core::DirEntry {
            name: f,
            is_dir: false,
            is_symlink: false,
            link_target: None,
            location: core::EntryLocation::Container {
                kind,
                archive_path: archive_path.to_path_buf(),
                inner_path: inner,
            },
            size,
            modified: None,
        });
    }

    entries
}

fn load_container_directory_async(
    app: &mut app_state::AppState,
    kind: core::ContainerKind,
    archive_path: PathBuf,
    cwd: String,
    root_hint: Option<String>,
    target_panel: core::ActivePanel,
    prefer_name: Option<String>,
    cache_mode: ContainerLoadMode,
) {
    app.stash_container_cache(target_panel);
    let cache_key = (archive_path.clone(), cwd.clone(), kind);
    let mut cached = app.container_dir_cache.remove(&cache_key);
    if cache_mode == ContainerLoadMode::ForceReload {
        cached = None;
        app.archive_index.remove(&archive_path);
    }
    let mut root_hint = root_hint.or_else(|| cached.as_ref().and_then(|cache| cache.root.clone()));
    let mut initial: Vec<core::DirEntry> = if let Some(ref cache) = cached {
        cache.entries.clone()
    } else {
        Vec::new()
    };
    let cached_selection = cached
        .as_ref()
        .map(|cache| (cache.selected_index, cache.top_index));
    if initial.is_empty() {
        if !cwd.is_empty() {
            let mut parent = cwd
                .trim_end_matches('/')
                .rsplit_once('/')
                .map(|(p, _)| p.to_string())
                .unwrap_or_default();
            if let Some(ref root) = root_hint
                && parent == *root
            {
                parent.clear();
            }
            initial.push(core::DirEntry {
                name: "..".into(),
                is_dir: true,
                is_symlink: false,
                link_target: None,
                location: core::EntryLocation::Container {
                    kind,
                    archive_path: archive_path.clone(),
                    inner_path: parent,
                },
                size: None,
                modified: None,
            });
        } else {
            let parent = archive_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf();
            initial.push(core::DirEntry {
                name: "..".into(),
                is_dir: true,
                is_symlink: false,
                link_target: None,
                location: core::EntryLocation::Fs(parent),
                size: None,
                modified: None,
            });
        }
    }

    let resume_rx = cached.as_mut().and_then(|cache| cache.entries_rx.take());
    let skip_loading = resume_rx.is_some() || cached.as_ref().is_some_and(|c| !c.loading);
    let wake = app.wake.clone();

    // Check if a shared index already exists (even incomplete)
    let mut used_index = false;
    let mut watching = false;
    if !skip_loading && let Some(shared) = app.archive_index.get(&archive_path).cloned() {
        let idx = shared.lock().unwrap();
        let mut listing = build_listing_from_index(&idx, &archive_path, kind, &cwd);
        let browser = app.panel(target_panel).browser();
        sort_entries(&mut listing, browser.sort_mode, browser.sort_desc);
        root_hint = idx.root.clone();
        let entry_count = idx.entries.len();
        let complete = idx.complete;
        drop(idx);
        initial = listing;
        used_index = true;
        if !complete {
            watching = true;
        }
        let _ = entry_count; // used below for index_last_seen
    }

    // Need to spawn a new loader thread
    if !skip_loading && !used_index {
        // Create the shared index
        let shared = Arc::new(std::sync::Mutex::new(app_state::ArchiveFullIndex {
            entries: Vec::new(),
            root: root_hint.clone(),
            complete: false,
        }));
        app.archive_index
            .insert(archive_path.clone(), shared.clone());
        watching = true;

        let archive_clone = archive_path.clone();
        let kind_clone = kind;
        let wake = wake.clone();

        thread::spawn(move || {
            let mut implicit_root: Option<String> = None;
            let mut decided = false;
            const BATCH: usize = 200;
            const DECIDE_LIMIT: usize = 64;
            let mut buffered: Vec<(String, bool, Option<u64>)> = Vec::new();
            let mut root_candidate: Option<String> = None;
            let mut seen_root_file = false;
            let mut seen_other_root = false;
            let mut batch_buf: Vec<(String, bool, Option<u64>)> = Vec::new();

            let file = match std::fs::File::open(&archive_clone) {
                Ok(file) => file,
                Err(_e) => {
                    let mut idx = shared.lock().unwrap();
                    idx.complete = true;
                    if let Some(ref wake) = wake {
                        wake();
                    }
                    return;
                }
            };

            // Closure to decide implicit root from buffered entries
            let decide_root = |root_candidate: &Option<String>,
                               seen_root_file: bool,
                               seen_other_root: bool|
             -> Option<String> {
                if !seen_root_file && !seen_other_root {
                    root_candidate.clone()
                } else {
                    None
                }
            };

            // Process a single entry name for root detection (pre-decide phase)
            fn detect_root_from_entry(
                name: &str,
                is_dir: bool,
                root_candidate: &mut Option<String>,
                seen_root_file: &mut bool,
                seen_other_root: &mut bool,
            ) {
                if let Some(slash) = name.find('/') {
                    let root = &name[..slash];
                    match root_candidate.as_ref() {
                        None => *root_candidate = Some(root.to_string()),
                        Some(existing) if existing != root => *seen_other_root = true,
                        _ => {}
                    }
                } else if is_dir {
                    match root_candidate.as_ref() {
                        None => *root_candidate = Some(name.to_string()),
                        Some(existing) if existing != name => *seen_other_root = true,
                        _ => {}
                    }
                } else {
                    *seen_root_file = true;
                }
            }

            fn flush_batch(
                shared: &std::sync::Mutex<app_state::ArchiveFullIndex>,
                batch: &mut Vec<(String, bool, Option<u64>)>,
                root: &Option<String>,
                wake: &Option<Arc<dyn Fn() + Send + Sync>>,
            ) {
                if batch.is_empty() {
                    return;
                }
                let mut idx = shared.lock().unwrap();
                idx.entries.append(batch);
                if idx.root.is_none() && root.is_some() {
                    idx.root = root.clone();
                }
                drop(idx);
                if let Some(wake) = wake {
                    wake();
                }
            }

            if kind_clone == core::ContainerKind::Zip {
                let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
                let mut zip = match zip::ZipArchive::new(reader) {
                    Ok(zip) => zip,
                    Err(_e) => {
                        let mut idx = shared.lock().unwrap();
                        idx.complete = true;
                        if let Some(ref wake) = wake {
                            wake();
                        }
                        return;
                    }
                };
                for i in 0..zip.len() {
                    let entry = match zip.by_index(i) {
                        Ok(entry) => entry,
                        Err(_) => continue,
                    };
                    let entry_is_dir = entry.is_dir();
                    let entry_size = if entry_is_dir {
                        None
                    } else {
                        Some(entry.size())
                    };
                    let name = core::normalize_archive_path(Path::new(entry.name()));
                    if name.is_empty() {
                        continue;
                    }

                    if !decided {
                        buffered.push((name.clone(), entry_is_dir, entry_size));
                        detect_root_from_entry(
                            &name,
                            entry_is_dir,
                            &mut root_candidate,
                            &mut seen_root_file,
                            &mut seen_other_root,
                        );
                        if buffered.len() >= DECIDE_LIMIT || seen_root_file || seen_other_root {
                            decided = true;
                            implicit_root =
                                decide_root(&root_candidate, seen_root_file, seen_other_root);
                            batch_buf.append(&mut buffered);
                            flush_batch(&shared, &mut batch_buf, &implicit_root, &wake);
                        }
                    } else {
                        batch_buf.push((name, entry_is_dir, entry_size));
                        if batch_buf.len() >= BATCH {
                            flush_batch(&shared, &mut batch_buf, &implicit_root, &wake);
                        }
                    }
                }
            } else {
                let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
                let reader: Box<dyn Read> = match kind_clone {
                    core::ContainerKind::TarBz2 => Box::new(bzip2::read::BzDecoder::new(reader)),
                    core::ContainerKind::TarGz => Box::new(flate2::read::GzDecoder::new(reader)),
                    core::ContainerKind::Tar => Box::new(reader),
                    _ => unreachable!(),
                };
                let mut archive = tar::Archive::new(reader);
                let entries = match archive.entries() {
                    Ok(entries) => entries,
                    Err(_e) => {
                        let mut idx = shared.lock().unwrap();
                        idx.complete = true;
                        if let Some(ref wake) = wake {
                            wake();
                        }
                        return;
                    }
                };

                for entry in entries.flatten() {
                    let path = match entry.path() {
                        Ok(path) => path,
                        Err(_) => continue,
                    };
                    let entry_is_dir = entry.header().entry_type().is_dir();
                    let entry_size = if entry_is_dir {
                        None
                    } else {
                        Some(entry.size())
                    };
                    let name = fileman::core::normalize_archive_path(&path);
                    if name.is_empty() {
                        continue;
                    }

                    if !decided {
                        buffered.push((name.clone(), entry_is_dir, entry_size));
                        detect_root_from_entry(
                            &name,
                            entry_is_dir,
                            &mut root_candidate,
                            &mut seen_root_file,
                            &mut seen_other_root,
                        );
                        if buffered.len() >= DECIDE_LIMIT || seen_root_file || seen_other_root {
                            decided = true;
                            implicit_root =
                                decide_root(&root_candidate, seen_root_file, seen_other_root);
                            batch_buf.append(&mut buffered);
                            flush_batch(&shared, &mut batch_buf, &implicit_root, &wake);
                        }
                    } else {
                        batch_buf.push((name, entry_is_dir, entry_size));
                        if batch_buf.len() >= BATCH {
                            flush_batch(&shared, &mut batch_buf, &implicit_root, &wake);
                        }
                    }
                }
            }

            // Flush remaining buffered entries (pre-decide phase never triggered)
            if !decided {
                implicit_root = decide_root(&root_candidate, seen_root_file, seen_other_root);
                batch_buf.append(&mut buffered);
            }
            // Flush remaining batch
            flush_batch(&shared, &mut batch_buf, &implicit_root, &wake);

            // Mark complete
            {
                let mut idx = shared.lock().unwrap();
                if idx.root.is_none() && implicit_root.is_some() {
                    idx.root = implicit_root;
                }
                idx.complete = true;
            }
            if let Some(ref wake) = wake {
                wake();
            }
        });
    }

    let remembered = prefer_name.clone().or_else(|| {
        app.container_last_selected_name
            .get(&(archive_path.clone(), cwd.clone(), kind))
            .cloned()
    });

    // Get current index entry count for index_last_seen
    let index_entry_count = if used_index || watching {
        app.archive_index
            .get(&archive_path)
            .map(|shared| shared.lock().unwrap().entries.len())
            .unwrap_or(0)
    } else {
        0
    };

    let panel_state = app.panel_mut(target_panel);
    let browser = panel_state.browser_mut();
    let initial_loading = cached
        .as_ref()
        .map(|cache| cache.loading)
        .unwrap_or(!skip_loading);

    let same_dir = browser.current_path == archive_path
        && matches!(&browser.browser_mode, core::BrowserMode::Container { cwd: old_cwd, .. } if *old_cwd == cwd);
    if !same_dir {
        browser.marked.clear();
    }
    browser.current_path = archive_path.clone();
    browser.browser_mode = core::BrowserMode::Container {
        kind,
        archive_path: archive_path.clone(),
        cwd: cwd.clone(),
        root: root_hint.clone(),
    };
    browser.container_root = root_hint;
    browser.entries = initial;
    if let Some((selected_index, top_index)) = cached_selection {
        browser.selected_index = selected_index.min(browser.entries.len().saturating_sub(1));
        browser.top_index = top_index.min(browser.selected_index);
    } else {
        browser.selected_index = 0;
        browser.top_index = 0;
    }
    browser.inline_rename = None;
    browser.dir_token = browser.dir_token.wrapping_add(1);
    browser.entries_rx = resume_rx.or(None);
    browser.prefer_select_name = remembered;
    browser.watching_archive = if watching {
        Some(archive_path.clone())
    } else {
        None
    };
    browser.index_last_seen = index_entry_count;
    browser.loading = if used_index && !watching {
        false
    } else {
        initial_loading
    };
    browser.loading_progress = if used_index {
        None
    } else {
        cached.and_then(|cache| cache.loading_progress)
    };
}

fn should_show_preview(app: &app_state::AppState, panel_side: core::ActivePanel) -> bool {
    let app_state::PanelState { mode, .. } = app.panel(panel_side);
    matches!(mode, app_state::PanelMode::Preview(_))
}

fn should_show_editor(app: &app_state::AppState, panel_side: core::ActivePanel) -> bool {
    let app_state::PanelState { mode, .. } = app.panel(panel_side);
    matches!(mode, app_state::PanelMode::Edit(_))
}

fn window_rows_for(panel_height: f32, spacing: f32) -> usize {
    let row = ROW_HEIGHT + spacing;
    if panel_height <= 0.0 || row <= 0.0 {
        return 10;
    }
    ((panel_height / row).floor() as usize).max(1)
}

fn active_window_rows(app: &app_state::AppState, cache: &UiCache) -> usize {
    match app.active_panel {
        core::ActivePanel::Left => cache.left_rows,
        core::ActivePanel::Right => cache.right_rows,
    }
}

fn open_search(app: &mut app_state::AppState, mode: core::SearchMode) {
    app.search_ui = app_state::SearchUiState::Open;
    app.search_focus = true;
    app.search_mode = mode;
}

fn preview_find_next(app: &mut app_state::AppState) {
    let Some(preview) = app.preview_panel_mut() else {
        return;
    };
    let Some(core::PreviewContent::Text(text)) = preview.content.as_ref() else {
        return;
    };
    let query = preview.find_query.trim();
    if query.is_empty() {
        return;
    }
    let lower_text = text.to_ascii_lowercase();
    let lower_query = query.to_ascii_lowercase();
    let start = preview.find_index.min(lower_text.len());
    let mut found = lower_text[start..].find(&lower_query).map(|i| i + start);
    if found.is_none() && start > 0 {
        found = lower_text.find(&lower_query);
    }
    if let Some(idx) = found {
        preview.find_index = idx + lower_query.len();
        let line = text[..idx].bytes().filter(|b| *b == b'\n').count();
        let line_height = preview.line_height.max(14.0);
        preview.scroll = line as f32 * line_height;
    }
}

fn apply_panel_snapshot(
    app: &mut app_state::AppState,
    which: core::ActivePanel,
    snapshot: fileman::app_state::PanelSnapshot,
) {
    match snapshot.mode {
        core::BrowserMode::Fs => {
            load_fs_directory_async(app, snapshot.current_path, which, snapshot.selected_name);
        }
        core::BrowserMode::Container {
            kind,
            archive_path,
            cwd,
            root,
        } => {
            load_container_directory_async(
                app,
                kind,
                archive_path,
                cwd,
                root,
                which,
                snapshot.selected_name,
                ContainerLoadMode::UseCache,
            );
        }
        core::BrowserMode::Search { .. } => {
            let results = app.search_results.clone();
            let panel = app.panel_mut(which);
            let browser = panel.browser_mut();
            browser.browser_mode = snapshot.mode;
            browser.current_path = snapshot.current_path;
            browser.entries.clear();
            browser.entries.extend(results.iter().map(|result| {
                let app_state::BrowserState {
                    browser_mode: ref mode,
                    ..
                } = *browser;
                let display_name = match mode {
                    core::BrowserMode::Search { root, .. } => result
                        .path
                        .strip_prefix(root)
                        .ok()
                        .and_then(|p| p.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            result
                                .path
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("<unknown>")
                                .to_string()
                        }),
                    _ => result
                        .path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("<unknown>")
                        .to_string(),
                };
                core::DirEntry {
                    name: display_name,
                    is_dir: result.is_dir,
                    is_symlink: false,
                    link_target: None,
                    location: core::EntryLocation::Fs(result.path.clone()),
                    size: result.size,
                    modified: result.modified,
                }
            }));
            sort_entries(&mut browser.entries, browser.sort_mode, browser.sort_desc);
            browser.entries_rx = None;
            browser.selected_index = snapshot
                .selected_name
                .and_then(|name| {
                    if let Some(path) = name.strip_prefix("fs:") {
                        return browser.entries.iter().position(|e| {
                            if let core::EntryLocation::Fs(p) = &e.location {
                                p.to_string_lossy() == path
                            } else {
                                false
                            }
                        });
                    }
                    browser.entries.iter().position(|e| e.name == name)
                })
                .unwrap_or(0);
            browser.top_index = 0;
            browser.loading = false;
            browser.loading_progress = None;
            browser.dir_token = browser.dir_token.wrapping_add(1);
        }
    }
}

fn cancel_search(app: &mut app_state::AppState) {
    app.search_request_id = app.search_request_id.wrapping_add(1);
    app.search_status = app_state::SearchStatus::Idle;
}

fn start_search(app: &mut app_state::AppState) {
    let needle = app.search_query.trim().to_string();
    if needle.is_empty() {
        return;
    }
    let search_mode = app.search_mode;
    let search_case = app.search_case;
    let id = app.search_request_id.wrapping_add(1);
    app.search_request_id = id;
    app.search_results.clear();
    app.search_selected = 0;
    app.search_status = app_state::SearchStatus::Running(core::SearchProgress {
        scanned: 0,
        matched: 0,
    });
    let root = {
        let panel = app.get_active_panel();
        let browser = panel.browser();
        browser.current_path.clone()
    };
    {
        app.push_history(app.active_panel);
        let panel = app.get_active_panel_mut();
        let browser = panel.browser_mut();
        browser.current_path = root.clone();
        browser.browser_mode = core::BrowserMode::Search {
            root: root.clone(),
            query: needle.clone(),
            mode: search_mode,
            case: search_case,
        };
        browser.entries.clear();
        browser.entries_rx = None;
        browser.selected_index = 0;
        browser.top_index = 0;
        browser.loading = true;
        browser.loading_progress = Some((0, None));
        browser.dir_token = browser.dir_token.wrapping_add(1);
        panel.mode = app_state::PanelMode::Browser;
    }
    let _ = app.search_tx.send(core::SearchRequest {
        id,
        root,
        needle,
        case: search_case,
        mode: search_mode,
    });
}

fn refresh_active_panel(app: &mut app_state::AppState) {
    let which = app.active_panel;
    let panel = app.panel(which);
    let browser = panel.browser();
    let path = browser.current_path.clone();
    if matches!(browser.browser_mode, core::BrowserMode::Fs) {
        load_fs_directory_async(app, path, which, None);
    }
}

fn refresh_fs_panels(app: &mut app_state::AppState) {
    for which in [core::ActivePanel::Left, core::ActivePanel::Right] {
        let browser = app.panel(which).browser();
        if !matches!(browser.browser_mode, core::BrowserMode::Fs) {
            continue;
        }
        let path = browser.current_path.clone();
        let current_name = browser
            .entries
            .get(browser.selected_index)
            .map(|e| e.name.clone());
        load_fs_directory_async(app, path, which, current_name);
    }
}

fn reload_panel(app: &mut app_state::AppState, which: core::ActivePanel) {
    let (mode, current_path, selected_name) = {
        let panel = app.panel(which);
        let browser = panel.browser();
        (
            browser.browser_mode.clone(),
            browser.current_path.clone(),
            browser
                .entries
                .get(browser.selected_index)
                .map(|entry| entry.name.clone()),
        )
    };
    match mode {
        core::BrowserMode::Fs => load_fs_directory_async(app, current_path, which, selected_name),
        core::BrowserMode::Container {
            kind,
            archive_path,
            cwd,
            root,
        } => load_container_directory_async(
            app,
            kind,
            archive_path,
            cwd,
            root,
            which,
            selected_name,
            ContainerLoadMode::ForceReload,
        ),
        core::BrowserMode::Search { .. } => {
            let results = app.search_results.clone();
            let panel = app.panel_mut(which);
            let browser = panel.browser_mut();
            rebuild_search_entries(browser, &results);
            if let Some(name) = selected_name
                && let Some(idx) = browser.entries.iter().position(|entry| entry.name == name)
            {
                browser.selected_index = idx;
            }
            if browser.selected_index < browser.top_index {
                browser.top_index = browser.selected_index;
            }
        }
    }
}

#[cfg(unix)]
fn open_props_dialog(app: &mut app_state::AppState) {
    let panel = app.get_active_panel();
    let browser = panel.browser();
    if !matches!(browser.browser_mode, core::BrowserMode::Fs) {
        return;
    }
    if browser.entries.is_empty() {
        return;
    }
    let entry = &browser.entries[browser.selected_index];
    if entry.name == ".." {
        return;
    }
    let core::EntryLocation::Fs(path) = &entry.location else {
        return;
    };
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) => {
            eprintln!("Failed to read metadata: {e}");
            return;
        }
    };
    let mode = meta.mode();
    let uid = meta.uid();
    let gid = meta.gid();
    let file_type = file_type_label(&meta);
    let is_dir = meta.is_dir();
    let user_label = users::get_user_by_uid(uid)
        .map(|user| user.name().to_string_lossy().into_owned())
        .unwrap_or_else(|| uid.to_string());
    let group_label = users::get_group_by_gid(gid)
        .map(|group| group.name().to_string_lossy().into_owned())
        .unwrap_or_else(|| gid.to_string());

    app.props_dialog = Some(app_state::PropsDialog {
        target: path.clone(),
        original: app_state::FileProps {
            mode,
            uid,
            gid,
            file_type,
            is_dir,
            user_label: user_label.clone(),
            group_label: group_label.clone(),
        },
        current: app_state::FilePropsEdit {
            mode: mode & 0o777,
            user: user_label,
            group: group_label,
        },
        error: None,
    });
}

#[cfg(unix)]
fn file_type_label(meta: &std::fs::Metadata) -> String {
    let file_type = meta.file_type();
    if file_type.is_dir() {
        "Directory".to_string()
    } else if file_type.is_file() {
        "Regular file".to_string()
    } else if file_type.is_symlink() {
        "Symlink".to_string()
    } else if file_type.is_block_device() {
        "Block device".to_string()
    } else if file_type.is_char_device() {
        "Character device".to_string()
    } else if file_type.is_fifo() {
        "FIFO".to_string()
    } else if file_type.is_socket() {
        "Socket".to_string()
    } else {
        "Unknown".to_string()
    }
}

fn make_whitespace_visible(text: &str) -> String {
    text.replace('\t', "→   ")
        .lines()
        .map(|line| format!("{line}⏎"))
        .collect::<Vec<_>>()
        .join("\n")
}

struct Runtime {
    window: winit::window::Window,
    window_id: winit::window::WindowId,
    context: bg::Context,
    surface: blade_graphics::Surface,
    surface_config: bg::SurfaceConfig,
    surface_info: blade_graphics::SurfaceInfo,
    command_encoder: blade_graphics::CommandEncoder,
    last_sync: Option<blade_graphics::SyncPoint>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    painter: be::GuiPainter,
    size: winit::dpi::PhysicalSize<u32>,
    app: app_state::AppState,
    ui_cache: UiCache,
    image_cache: ImageCache,
    highlight_cache: HashMap<String, egui::text::LayoutJob>,
    highlight_pending: HashSet<String>,
    highlight_req_tx: mpsc::Sender<HighlightRequest>,
    highlight_res_rx: mpsc::Receiver<HighlightResult>,
    highlight_results: VecDeque<HighlightResult>,
    image_req_tx: mpsc::Sender<ImageRequest>,
    image_res_rx: mpsc::Receiver<ImageResponse>,
    image_pending: VecDeque<ImageResponse>,
    needs_redraw: bool,
}

impl Runtime {
    fn shutdown(&mut self) {
        self.image_cache.textures.clear();
        self.image_cache.meta.clear();
        self.image_cache.failures.clear();
        self.image_cache.order.clear();
        self.image_cache.pending.clear();
        self.image_cache.refining.clear();
        self.highlight_cache.clear();
        self.highlight_pending.clear();
        if let Some(sync) = self.last_sync.take() {
            self.context.wait_for(&sync, !0);
        }
        self.context
            .destroy_command_encoder(&mut self.command_encoder);
        self.painter.destroy(&self.context);
        self.context.destroy_surface(&mut self.surface);
    }
}

struct App {
    runtime: Option<Runtime>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    start_dir: Option<PathBuf>,
}

impl App {
    fn new(
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
        start_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            runtime: None,
            proxy,
            start_dir,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum UserEvent {
    Wake,
}

impl winit::application::ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.runtime.is_some() {
            return;
        }

        let window = event_loop
            .create_window(
                winit::window::WindowAttributes::default()
                    .with_title("Fileman (egui)")
                    .with_window_icon(app_icon()),
            )
            .expect("create window");
        let window_id = window.id();

        let context = unsafe {
            match bg::Context::init(bg::ContextDesc {
                presentation: true,
                xr: None,
                ray_tracing: false,
                validation: cfg!(debug_assertions),
                timing: false,
                capture: false,
                overlay: true,
                device_id: 0,
            }) {
                Ok(context) => context,
                Err(err) => {
                    eprintln!("Failed to init GPU context: {err:?}");
                    eprintln!("{}", surface_error_help());
                    event_loop.exit();
                    return;
                }
            }
        };
        let size = window.inner_size();
        let surface_config = bg::SurfaceConfig {
            size: bg::Extent {
                width: size.width.max(1),
                height: size.height.max(1),
                depth: 1,
            },
            usage: bg::TextureUsage::TARGET,
            ..bg::SurfaceConfig::default()
        };
        let surface = match context.create_surface_configured(&window, surface_config) {
            Ok(surface) => surface,
            Err(err) => {
                eprintln!("Failed to create GPU surface: {err:?}");
                eprintln!("{}", surface_error_help());
                event_loop.exit();
                return;
            }
        };
        let surface_info = surface.info();

        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );

        let painter = be::GuiPainter::new(surface_info, &context);
        let command_encoder = context.create_command_encoder(bg::CommandEncoderDesc {
            name: "egui",
            buffer_count: 1,
        });

        let cur_dir = self
            .start_dir
            .take()
            .unwrap_or_else(|| std::env::current_dir().expect("current_dir"));
        let (io_tx, io_rx, io_cancel_tx) = workers::start_io_worker();
        let (preview_tx, preview_rx) = workers::start_preview_worker(Some(Arc::new({
            let proxy = self.proxy.clone();
            move || {
                let _ = proxy.send_event(UserEvent::Wake);
            }
        })));
        let (dir_size_tx, dir_size_rx) = workers::start_dir_size_worker();
        let (search_tx, search_rx) = workers::start_search_worker();
        let (image_req_tx, image_req_rx) = mpsc::channel::<ImageRequest>();
        let (image_res_tx, image_res_rx) = mpsc::channel::<ImageResponse>();
        let (highlight_req_tx, highlight_req_rx) = mpsc::channel::<HighlightRequest>();
        let (highlight_res_tx, highlight_res_rx) = mpsc::channel::<HighlightResult>();
        let (edit_tx, edit_rx) = mpsc::channel::<core::EditLoadRequest>();
        let (edit_res_tx, edit_res_rx) = mpsc::channel::<core::EditLoadResult>();

        // Full-decode thread: handles expensive tier-3 decodes so the
        // fast preview thread stays responsive for tier 1/2.
        let (full_decode_tx, full_decode_rx) = mpsc::channel::<(String, Vec<u8>)>();
        let full_res_tx = image_res_tx.clone();
        let full_proxy = self.proxy.clone();
        thread::spawn(move || {
            while let Ok((mut key, mut data)) = full_decode_rx.recv() {
                // Drain stale: only decode the latest request
                while let Ok((newer_key, newer_data)) = full_decode_rx.try_recv() {
                    key = newer_key;
                    data = newer_data;
                }
                if let Some((decoded, meta)) =
                    image_decode::decode_image_bytes(&data, MAX_TEXTURE_SIDE)
                {
                    let refining = false;
                    let _ = full_res_tx.send(ImageResponse::Ok(ImageResult {
                        key,
                        image: decoded,
                        meta,
                        refining,
                    }));
                }
                let _ = full_proxy.send_event(UserEvent::Wake);
            }
        });

        // Fast preview thread: reads files, sends tier 1/2 instantly,
        // then forwards to the full-decode thread for tier 3.
        let proxy = self.proxy.clone();
        thread::spawn(move || {
            while let Ok(mut req) = image_req_rx.recv() {
                // Skip stale requests
                while let Ok(newer) = image_req_rx.try_recv() {
                    req = newer;
                }

                let raw_bytes: Option<Vec<u8>> = match req.source {
                    ImageSource::Fs(ref path) => std::fs::read(path).ok(),
                    ImageSource::Container {
                        kind,
                        ref archive_path,
                        ref inner_path,
                    } => fileman::core::read_container_bytes_prefix(
                        kind,
                        archive_path,
                        inner_path,
                        usize::MAX,
                    )
                    .ok(),
                };
                let Some(data) = raw_bytes else {
                    let _ = image_res_tx.send(ImageResponse::Err {
                        key: req.key,
                        message: "Failed to read image data".to_string(),
                    });
                    let _ = proxy.send_event(UserEvent::Wake);
                    continue;
                };

                // For animated GIFs: send the first frame immediately,
                // forward full decode to the dedicated thread.
                if image_decode::is_gif(&data) {
                    if let Some((first, meta)) =
                        image_decode::decode_gif_first_frame(&data, MAX_TEXTURE_SIDE)
                    {
                        let _ = image_res_tx.send(ImageResponse::Ok(ImageResult {
                            key: req.key.clone(),
                            image: image_decode::DecodedImage::Static(first),
                            meta,
                            refining: true,
                        }));
                        let _ = proxy.send_event(UserEvent::Wake);
                    }
                    let _ = full_decode_tx.send((req.key, data));
                    continue;
                }

                // For JPEGs: three-tier progressive loading:
                //   1. EXIF thumbnail (instant, ~160×120)
                //   2. DC-only 1/8-scale (fast, parses entropy stream)
                //   3. Full decode (forwarded to dedicated thread)
                if image_decode::is_jpeg(&data) {
                    // Tier 1: EXIF thumbnail — near-instant
                    if let Some((thumb, thumb_meta)) =
                        image_decode::decode_jpeg_exif_thumbnail(&data, MAX_TEXTURE_SIDE)
                    {
                        let _ = image_res_tx.send(ImageResponse::Ok(ImageResult {
                            key: req.key.clone(),
                            image: thumb,
                            meta: thumb_meta,
                            refining: true,
                        }));
                        let _ = proxy.send_event(UserEvent::Wake);
                    }
                    // Tier 2: DC-only 1/8 scale — good quality preview
                    if let Some((dc, dc_meta)) =
                        image_decode::decode_jpeg_dc_preview(&data, MAX_TEXTURE_SIDE)
                    {
                        let _ = image_res_tx.send(ImageResponse::Ok(ImageResult {
                            key: req.key.clone(),
                            image: dc,
                            meta: dc_meta,
                            refining: true,
                        }));
                        let _ = proxy.send_event(UserEvent::Wake);
                    }
                    // Tier 3: forward to full-decode thread
                    let _ = full_decode_tx.send((req.key, data));
                    continue;
                }

                // Other formats: decode directly (typically fast)
                if let Some((decoded, meta)) =
                    image_decode::decode_image_bytes(&data, MAX_TEXTURE_SIDE)
                {
                    let _ = image_res_tx.send(ImageResponse::Ok(ImageResult {
                        key: req.key,
                        image: decoded,
                        meta,
                        refining: false,
                    }));
                } else {
                    let _ = image_res_tx.send(ImageResponse::Err {
                        key: req.key,
                        message: "Unsupported image format".to_string(),
                    });
                }
                let _ = proxy.send_event(UserEvent::Wake);
            }
        });

        let proxy = self.proxy.clone();
        thread::spawn(move || {
            while let Ok(req) = highlight_req_rx.recv() {
                let job = highlight_text_job(&req.text, req.ext.as_deref(), req.theme_kind);
                let _ = highlight_res_tx.send(HighlightResult { key: req.key, job });
                let _ = proxy.send_event(UserEvent::Wake);
            }
        });

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
                tabs: vec![app_state::BrowserState {
                    browser_mode: core::BrowserMode::Fs,
                    current_path: cur_dir.clone(),
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
                }],
                active_tab: 0,
                mode: app_state::PanelMode::Browser,
            },
            right_panel: app_state::PanelState {
                tabs: vec![app_state::BrowserState {
                    browser_mode: core::BrowserMode::Fs,
                    current_path: cur_dir.clone(),
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
                }],
                active_tab: 0,
                mode: app_state::PanelMode::Browser,
            },
            active_panel: core::ActivePanel::Left,
            allow_external_open: true,
            preview_return_focus: None,
            wake: Some(Arc::new({
                let proxy = self.proxy.clone();
                move || {
                    let _ = proxy.send_event(UserEvent::Wake);
                }
            })),
            preview_tx: preview_tx.clone(),
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
        load_fs_directory_async(&mut app, cur_dir.clone(), core::ActivePanel::Left, None);
        load_fs_directory_async(&mut app, cur_dir, core::ActivePanel::Right, None);

        #[cfg(feature = "self-update")]
        {
            let (update_tx, update_rx) = mpsc::channel();
            app.update_status = app_state::UpdateStatus::Checking;
            app.update_rx = Some(update_rx);
            let wake = app.wake.clone();
            thread::spawn(move || {
                let status = match update::check_for_update() {
                    Ok(Some(release)) => {
                        app_state::UpdateStatus::Available(release.version.to_string())
                    }
                    Ok(None) => app_state::UpdateStatus::UpToDate,
                    Err(e) => app_state::UpdateStatus::Failed(e.to_string()),
                };
                let _ = update_tx.send(status);
                if let Some(ref w) = wake {
                    w();
                }
            });
        }

        let ui_cache = UiCache {
            left_rows: 10,
            right_rows: 10,
            scroll_mode: ScrollMode::Default,
            last_left_selected: 0,
            last_right_selected: 0,
            last_active_panel: core::ActivePanel::Left,
            last_left_dir_token: 0,
            last_right_dir_token: 0,
        };
        let image_cache = ImageCache {
            textures: HashMap::new(),
            animations: HashMap::new(),
            meta: HashMap::new(),
            failures: HashMap::new(),
            pending: HashSet::new(),
            refining: HashMap::new(),
            order: VecDeque::new(),
        };
        let highlight_cache = HashMap::new();
        let highlight_pending = HashSet::new();

        self.runtime = Some(Runtime {
            window,
            window_id,
            context,
            surface,
            surface_config,
            surface_info,
            command_encoder,
            last_sync: None,
            egui_ctx,
            egui_state,
            painter,
            size,
            app,
            ui_cache,
            image_cache,
            highlight_cache,
            highlight_pending,
            highlight_req_tx,
            highlight_res_rx,
            highlight_results: VecDeque::new(),
            image_req_tx,
            image_res_rx,
            image_pending: VecDeque::new(),
            needs_redraw: true,
        });
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        let runtime = match self.runtime.as_mut() {
            Some(runtime) if runtime.window_id == window_id => runtime,
            _ => return,
        };

        match event {
            winit::event::WindowEvent::RedrawRequested => {
                let mut highlight_updated = false;
                let mut completed = 0usize;
                while runtime.app.io_rx.try_recv().is_ok() {
                    completed += 1;
                }
                if completed > 0 {
                    runtime.app.on_io_completed(completed);
                    refresh_fs_panels(&mut runtime.app);
                }
                let _ = pump_async(&mut runtime.app);
                let mut decoded_images = Vec::new();
                while decoded_images.len() < MAX_IMAGE_UPLOADS_PER_FRAME {
                    if let Some(img) = runtime.image_pending.pop_front() {
                        decoded_images.push(img);
                        continue;
                    }
                    match runtime.image_res_rx.try_recv() {
                        Ok(img) => decoded_images.push(img),
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    }
                }
                while let Some(res) = runtime.highlight_results.pop_front() {
                    runtime.highlight_cache.insert(res.key.clone(), res.job);
                    runtime.highlight_pending.remove(&res.key);
                    runtime.needs_redraw = true;
                    highlight_updated = true;
                }
                while let Ok(res) = runtime.highlight_res_rx.try_recv() {
                    runtime.highlight_cache.insert(res.key.clone(), res.job);
                    runtime.highlight_pending.remove(&res.key);
                    runtime.needs_redraw = true;
                    highlight_updated = true;
                }

                let mut raw_input = runtime.egui_state.take_egui_input(&runtime.window);
                // Save key events before stripping them from raw_input.
                // We handle all keyboard input ourselves; stripping prevents
                // egui's focus system from moving focus to sort controls etc.
                let key_events: Vec<egui::Event> = raw_input
                    .events
                    .iter()
                    .filter(|e| matches!(e, egui::Event::Key { .. }))
                    .cloned()
                    .collect();
                raw_input
                    .events
                    .retain(|e| !matches!(e, egui::Event::Key { .. }));
                let output = runtime.egui_ctx.run(raw_input, |ctx| {
                    // Inject key events back into InputState so our handler can read them
                    ctx.input_mut(|i| i.events.extend(key_events.iter().cloned()));
                    apply_theme(ctx, &runtime.app.theme.colors());
                    let input = ctx.input(|i| i.clone());
                    input::handle_keyboard(ctx, &input, &mut runtime.app, &mut runtime.ui_cache);
                    runtime.ui_cache.update_scroll_mode(&runtime.app);

                    // Defer results for keys whose current refining preview
                    // hasn't been shown long enough yet.
                    let now = std::time::Instant::now();
                    for decoded in decoded_images.drain(..) {
                        match decoded {
                            ImageResponse::Ok(ref inner)
                                if runtime.image_cache.refining.get(&inner.key).is_some_and(
                                    |t| now.duration_since(*t) < MIN_REFINING_DISPLAY,
                                ) =>
                            {
                                runtime.image_pending.push_back(decoded);
                                continue;
                            }
                            ImageResponse::Ok(decoded) => {
                                let first_frame = match decoded.image {
                                    image_decode::DecodedImage::Static(image) => {
                                        runtime.image_cache.animations.remove(&decoded.key);
                                        image
                                    }
                                    image_decode::DecodedImage::Animated(gif_frames) => {
                                        let delays: Vec<u32> =
                                            gif_frames.iter().map(|f| f.delay_ms).collect();
                                        let total: u64 = delays.iter().map(|d| *d as u64).sum();
                                        let frames: Vec<egui::ColorImage> =
                                            gif_frames.into_iter().map(|f| f.image).collect();
                                        let first = frames[0].clone();
                                        runtime.image_cache.animations.insert(
                                            decoded.key.clone(),
                                            AnimationData {
                                                frames,
                                                delays,
                                                total_duration_ms: total.max(1),
                                            },
                                        );
                                        first
                                    }
                                };
                                let handle = ctx.load_texture(
                                    format!("preview:{}", decoded.key),
                                    first_frame,
                                    egui::TextureOptions::LINEAR,
                                );
                                if !runtime.image_cache.textures.contains_key(&decoded.key) {
                                    runtime.image_cache.order.push_back(decoded.key.clone());
                                }
                                runtime
                                    .image_cache
                                    .textures
                                    .insert(decoded.key.clone(), handle);
                                runtime
                                    .image_cache
                                    .meta
                                    .insert(decoded.key.clone(), decoded.meta);
                                runtime.image_cache.pending.remove(&decoded.key);
                                runtime.image_cache.failures.remove(&decoded.key);
                                if decoded.refining {
                                    runtime
                                        .image_cache
                                        .refining
                                        .entry(decoded.key.clone())
                                        .or_insert(now);
                                } else {
                                    runtime.image_cache.refining.remove(&decoded.key);
                                }
                                // Ensure deferred results get processed soon
                                if !runtime.image_pending.is_empty() {
                                    ctx.request_repaint_after(MIN_REFINING_DISPLAY);
                                }
                                while runtime.image_cache.order.len() > MAX_IMAGE_TEXTURES {
                                    if let Some(old) = runtime.image_cache.order.pop_front()
                                        && old != decoded.key
                                    {
                                        runtime.image_cache.textures.remove(&old);
                                        runtime.image_cache.meta.remove(&old);
                                        runtime.image_cache.failures.remove(&old);
                                        runtime.image_cache.animations.remove(&old);
                                        runtime.image_cache.refining.remove(&old);
                                    }
                                }
                            }
                            ImageResponse::Err { key, message } => {
                                runtime.image_cache.pending.remove(&key);
                                runtime.image_cache.failures.insert(key, message);
                            }
                        }
                        runtime.needs_redraw = true;
                    }

                    runtime.app.refresh_tick = runtime.app.refresh_tick.wrapping_add(1);
                    ui::command_bar::draw_command_bar(
                        ctx,
                        &runtime.app,
                        &runtime.app.theme.colors(),
                    );

                    egui::CentralPanel::default().show(ctx, |ui| {
                        let rect = ui.available_rect_before_wrap();
                        let spacing_x = ui.spacing().item_spacing.x;
                        let panel_width = ((rect.width() - spacing_x) * 0.5).max(0.0);
                        let left_rect = egui::Rect::from_min_size(
                            rect.min,
                            egui::Vec2::new(panel_width, rect.height()),
                        );
                        let right_rect = egui::Rect::from_min_size(
                            rect.min + egui::Vec2::new(panel_width + spacing_x, 0.0),
                            egui::Vec2::new(panel_width, rect.height()),
                        );

                        ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
                            if should_show_editor(&runtime.app, core::ActivePanel::Left) {
                                let is_focused =
                                    runtime.app.active_panel == core::ActivePanel::Left;
                                let theme = runtime.app.theme.clone();
                                let panel = runtime.app.panel_mut(core::ActivePanel::Left);
                                if let app_state::PanelMode::Edit(ref mut edit) = panel.mode {
                                    ui::editor::draw_editor(
                                        ui,
                                        ui::editor::EditorRender {
                                            theme: &theme,
                                            is_focused,
                                            edit,
                                            highlight_cache: &runtime.highlight_cache,
                                            highlight_pending: &mut runtime.highlight_pending,
                                            highlight_req_tx: &runtime.highlight_req_tx,
                                            min_height: rect.height(),
                                        },
                                    );
                                }
                            } else if should_show_preview(&runtime.app, core::ActivePanel::Left) {
                                let is_focused =
                                    runtime.app.active_panel == core::ActivePanel::Left;
                                let theme = runtime.app.theme.clone();
                                let panel = runtime.app.panel_mut(core::ActivePanel::Left);
                                if let app_state::PanelMode::Preview(ref mut preview) = panel.mode {
                                    ui::preview::draw_preview(
                                        ui,
                                        ui::preview::PreviewRender {
                                            theme: &theme,
                                            is_focused,
                                            preview,
                                            image_cache: &mut runtime.image_cache,
                                            image_req_tx: &runtime.image_req_tx,
                                            highlight_cache: &runtime.highlight_cache,
                                            highlight_pending: &mut runtime.highlight_pending,
                                            highlight_req_tx: &runtime.highlight_req_tx,
                                            min_height: rect.height(),
                                        },
                                    );
                                }
                            } else if let Some(_help) =
                                runtime.app.help_panel(core::ActivePanel::Left)
                            {
                                let is_focused =
                                    runtime.app.active_panel == core::ActivePanel::Left;
                                let theme = runtime.app.theme.clone();
                                let async_status = runtime.app.async_status();
                                if ui::help::draw_help(ui, &theme, is_focused, rect.height(), &async_status) {
                                    start_install(&mut runtime.app);
                                }
                            } else {
                                runtime.ui_cache.left_rows = ui::panel::draw_panel(
                                    ui,
                                    &mut runtime.app,
                                    core::ActivePanel::Left,
                                    &mut runtime.image_cache,
                                    &runtime.image_req_tx,
                                    runtime.ui_cache.scroll_mode,
                                    rect.height(),
                                );
                            }
                        });
                        ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
                            if should_show_editor(&runtime.app, core::ActivePanel::Right) {
                                let is_focused =
                                    runtime.app.active_panel == core::ActivePanel::Right;
                                let theme = runtime.app.theme.clone();
                                let panel = runtime.app.panel_mut(core::ActivePanel::Right);
                                if let app_state::PanelMode::Edit(ref mut edit) = panel.mode {
                                    ui::editor::draw_editor(
                                        ui,
                                        ui::editor::EditorRender {
                                            theme: &theme,
                                            is_focused,
                                            edit,
                                            highlight_cache: &runtime.highlight_cache,
                                            highlight_pending: &mut runtime.highlight_pending,
                                            highlight_req_tx: &runtime.highlight_req_tx,
                                            min_height: rect.height(),
                                        },
                                    );
                                }
                            } else if should_show_preview(&runtime.app, core::ActivePanel::Right) {
                                let is_focused =
                                    runtime.app.active_panel == core::ActivePanel::Right;
                                let theme = runtime.app.theme.clone();
                                let panel = runtime.app.panel_mut(core::ActivePanel::Right);
                                if let app_state::PanelMode::Preview(ref mut preview) = panel.mode {
                                    ui::preview::draw_preview(
                                        ui,
                                        ui::preview::PreviewRender {
                                            theme: &theme,
                                            is_focused,
                                            preview,
                                            image_cache: &mut runtime.image_cache,
                                            image_req_tx: &runtime.image_req_tx,
                                            highlight_cache: &runtime.highlight_cache,
                                            highlight_pending: &mut runtime.highlight_pending,
                                            highlight_req_tx: &runtime.highlight_req_tx,
                                            min_height: rect.height(),
                                        },
                                    );
                                }
                            } else if let Some(_help) =
                                runtime.app.help_panel(core::ActivePanel::Right)
                            {
                                let is_focused =
                                    runtime.app.active_panel == core::ActivePanel::Right;
                                let theme = runtime.app.theme.clone();
                                let async_status = runtime.app.async_status();
                                if ui::help::draw_help(ui, &theme, is_focused, rect.height(), &async_status) {
                                    start_install(&mut runtime.app);
                                }
                            } else {
                                runtime.ui_cache.right_rows = ui::panel::draw_panel(
                                    ui,
                                    &mut runtime.app,
                                    core::ActivePanel::Right,
                                    &mut runtime.image_cache,
                                    &runtime.image_req_tx,
                                    runtime.ui_cache.scroll_mode,
                                    rect.height(),
                                );
                            }
                        });
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                rect.min + egui::Vec2::new(panel_width, 0.0),
                                egui::Vec2::new(spacing_x, rect.height()),
                            ),
                            egui::CornerRadius::ZERO,
                            color32(runtime.app.theme.colors().divider),
                        );
                    });

                    if runtime.app.theme_picker_open {
                        ui::theme_picker::draw_theme_picker(ctx, &mut runtime.app);
                    }
                    if runtime.app.pending_op.is_some() {
                        ui::modals::draw_confirmation(ctx, &mut runtime.app);
                    }
                    if let Some(edit) = runtime.app.edit_panel_mut()
                        && edit.confirm_discard
                    {
                        ui::modals::draw_discard_modal(ctx, &mut runtime.app);
                    }
                    #[cfg(unix)]
                    if runtime.app.props_dialog.is_some() {
                        ui::props_dialog::draw_props_modal(ctx, &mut runtime.app);
                    }
                    if runtime.app.io_in_flight > 0 {
                        ui::modals::draw_progress_modal(ctx, &runtime.app);
                    }
                });
                runtime
                    .egui_state
                    .handle_platform_output(&runtime.window, output.platform_output);
                for vo in output.viewport_output.values() {
                    if vo.repaint_delay.is_zero() {
                        runtime.needs_redraw = true;
                    }
                }

                let paint_jobs = runtime
                    .egui_ctx
                    .tessellate(output.shapes, output.pixels_per_point);
                let screen_descriptor = be::ScreenDescriptor {
                    physical_size: (runtime.size.width, runtime.size.height),
                    scale_factor: runtime.window.scale_factor() as f32,
                };

                if let Some(sync) = runtime.last_sync.take() {
                    runtime.context.wait_for(&sync, !0);
                }
                runtime.command_encoder.start();
                runtime.painter.update_textures(
                    &mut runtime.command_encoder,
                    &output.textures_delta,
                    &runtime.context,
                );

                let frame = runtime.surface.acquire_frame();
                runtime.command_encoder.init_texture(frame.texture());
                let view = runtime.context.create_texture_view(
                    frame.texture(),
                    bg::TextureViewDesc {
                        name: "surface",
                        format: runtime.surface_info.format,
                        dimension: bg::ViewDimension::D2,
                        subresources: &bg::TextureSubresources::default(),
                    },
                );

                let mut render = runtime.command_encoder.render(
                    "egui",
                    bg::RenderTargetSet {
                        colors: &[bg::RenderTarget {
                            view,
                            init_op: bg::InitOp::Clear(bg::TextureColor::TransparentBlack),
                            finish_op: bg::FinishOp::Store,
                        }],
                        depth_stencil: None,
                    },
                );
                runtime.painter.paint(
                    &mut render,
                    &paint_jobs,
                    &screen_descriptor,
                    &runtime.context,
                );
                drop(render);

                runtime.command_encoder.present(frame);
                let sync = runtime.context.submit(&mut runtime.command_encoder);
                runtime.last_sync = Some(sync.clone());
                runtime.painter.after_submit(&sync);
                runtime.context.destroy_texture_view(view);
                if highlight_updated {
                    runtime.window.request_redraw();
                }
            }
            other => {
                let is_key_release = matches!(
                    other,
                    winit::event::WindowEvent::KeyboardInput { ref event, .. }
                        if event.state == winit::event::ElementState::Released
                );
                let event_response = runtime.egui_state.on_window_event(&runtime.window, &other);
                if event_response.repaint && !is_key_release {
                    runtime.needs_redraw = true;
                }
                if event_response.consumed {
                    runtime.needs_redraw = false;
                    if !is_key_release {
                        runtime.window.request_redraw();
                    }
                    return;
                }

                match other {
                    winit::event::WindowEvent::CloseRequested => event_loop.exit(),
                    winit::event::WindowEvent::Resized(new_size) => {
                        runtime.size = new_size;
                        runtime.surface_config.size = bg::Extent {
                            width: runtime.size.width.max(1),
                            height: runtime.size.height.max(1),
                            depth: 1,
                        };
                        runtime
                            .context
                            .reconfigure_surface(&mut runtime.surface, runtime.surface_config);
                        runtime.needs_redraw = true;
                    }
                    _ => {}
                }
            }
        }
    }

    fn user_event(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop, _event: UserEvent) {
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.needs_redraw = true;
            runtime.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
        if let Some(runtime) = self.runtime.as_mut() {
            while let Ok(img) = runtime.image_res_rx.try_recv() {
                runtime.image_pending.push_back(img);
                runtime.needs_redraw = true;
            }
            while let Ok(res) = runtime.highlight_res_rx.try_recv() {
                runtime.highlight_results.push_back(res);
                runtime.needs_redraw = true;
            }
            if !runtime.highlight_results.is_empty() {
                runtime.window.request_redraw();
            }
            if let Some(preview) = runtime.app.preview_panel_mut()
                && let Some(core::PreviewContent::Image(path)) = preview.content.as_ref()
            {
                let key = match path {
                    core::ImageLocation::Fs(path) => path.to_string_lossy().into_owned(),
                    core::ImageLocation::Container {
                        kind,
                        archive_path,
                        inner_path,
                    } => format!(
                        "{}::{}:/{}",
                        archive_path.to_string_lossy(),
                        match kind {
                            core::ContainerKind::Zip => "zip",
                            core::ContainerKind::Tar => "tar",
                            core::ContainerKind::TarGz => "tar.gz",
                            core::ContainerKind::TarBz2 => "tar.bz2",
                        },
                        inner_path
                    ),
                };
                if runtime.image_cache.pending.contains(&key)
                    || !runtime.image_cache.textures.contains_key(&key)
                {
                    runtime.needs_redraw = true;
                }
            }
            if pump_async(&mut runtime.app) {
                runtime.needs_redraw = true;
            }
            if runtime.needs_redraw {
                runtime.window.request_redraw();
                runtime.needs_redraw = false;
            }
        }
    }

    fn exiting(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.shutdown();
        }
    }
}

#[derive(Default)]
struct CliArgs {
    snapshot: Option<PathBuf>,
    replay: Option<PathBuf>,
    start_dir: Option<PathBuf>,
    #[cfg(feature = "self-update")]
    update: bool,
}

fn parse_cli_args() -> anyhow::Result<CliArgs> {
    let mut args = std::env::args().skip(1);
    let mut parsed = CliArgs::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                eprintln!("fileman - a two-panel file manager");
                eprintln!();
                eprintln!("Usage: fileman [OPTIONS] [DIRECTORY]");
                eprintln!();
                eprintln!("Options:");
                eprintln!("  -h, --help         Show this help message");
                eprintln!("  --snapshot <PATH>   Render a snapshot to PNG");
                eprintln!("  --replay <PATH>     Replay an input recording");
                if cfg!(feature = "self-update") {
                    eprintln!("  --update            Check for updates and install");
                }
                std::process::exit(0);
            }
            "--snapshot" => {
                parsed.snapshot = Some(
                    args.next()
                        .map(PathBuf::from)
                        .ok_or_else(|| anyhow::anyhow!("--snapshot requires a path"))?,
                );
            }
            "--replay" => {
                parsed.replay = Some(
                    args.next()
                        .map(PathBuf::from)
                        .ok_or_else(|| anyhow::anyhow!("--replay requires a path"))?,
                );
            }
            #[cfg(feature = "self-update")]
            "--update" => {
                parsed.update = true;
            }
            other if !other.starts_with('-') => {
                let path = PathBuf::from(other);
                let path = if path.is_relative() {
                    std::env::current_dir()?.join(path)
                } else {
                    path
                };
                parsed.start_dir = Some(path);
            }
            other => {
                anyhow::bail!("Unknown option: {other}\nRun with --help for usage.");
            }
        }
    }
    Ok(parsed)
}

struct UiRender<'a> {
    ctx: &'a egui::Context,
    app: &'a mut app_state::AppState,
    ui_cache: &'a mut UiCache,
    image_cache: &'a mut ImageCache,
    image_req_tx: &'a mpsc::Sender<ImageRequest>,
    highlight_cache: &'a HashMap<String, egui::text::LayoutJob>,
    highlight_pending: &'a mut HashSet<String>,
    highlight_req_tx: &'a mpsc::Sender<HighlightRequest>,
}

fn draw_root_ui(render: UiRender<'_>) {
    let UiRender {
        ctx,
        app,
        ui_cache,
        image_cache,
        image_req_tx,
        highlight_cache,
        highlight_pending,
        highlight_req_tx,
    } = render;
    app.refresh_tick = app.refresh_tick.wrapping_add(1);
    apply_theme(ctx, &app.theme.colors());
    ui::command_bar::draw_command_bar(ctx, app, &app.theme.colors());
    egui::CentralPanel::default().show(ctx, |ui| {
        let rect = ui.available_rect_before_wrap();
        let spacing_x = ui.spacing().item_spacing.x;
        let panel_width = ((rect.width() - spacing_x) * 0.5).max(0.0);
        let left_rect =
            egui::Rect::from_min_size(rect.min, egui::Vec2::new(panel_width, rect.height()));
        let right_rect = egui::Rect::from_min_size(
            rect.min + egui::Vec2::new(panel_width + spacing_x, 0.0),
            egui::Vec2::new(panel_width, rect.height()),
        );

        ui_cache.left_rows = ui
            .scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
                if should_show_editor(app, core::ActivePanel::Left) {
                    let is_focused = app.active_panel == core::ActivePanel::Left;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(core::ActivePanel::Left);
                    if let app_state::PanelMode::Edit(ref mut edit) = panel.mode {
                        ui::editor::draw_editor(
                            ui,
                            ui::editor::EditorRender {
                                theme: &theme,
                                is_focused,
                                edit,
                                highlight_cache,
                                highlight_pending,
                                highlight_req_tx,
                                min_height: rect.height(),
                            },
                        );
                    }
                    ui_cache.left_rows
                } else if should_show_preview(app, core::ActivePanel::Left) {
                    let is_focused = app.active_panel == core::ActivePanel::Left;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(core::ActivePanel::Left);
                    if let app_state::PanelMode::Preview(ref mut preview) = panel.mode {
                        ui::preview::draw_preview(
                            ui,
                            ui::preview::PreviewRender {
                                theme: &theme,
                                is_focused,
                                preview,
                                image_cache,
                                image_req_tx,
                                highlight_cache,
                                highlight_pending,
                                highlight_req_tx,
                                min_height: rect.height(),
                            },
                        );
                    }
                    ui_cache.left_rows
                } else if let Some(_help) = app.help_panel(core::ActivePanel::Left) {
                    let is_focused = app.active_panel == core::ActivePanel::Left;
                    let theme = app.theme.clone();
                    let async_status = app.async_status();
                    ui::help::draw_help(ui, &theme, is_focused, rect.height(), &async_status);
                    ui_cache.left_rows
                } else {
                    ui::panel::draw_panel(
                        ui,
                        app,
                        core::ActivePanel::Left,
                        image_cache,
                        image_req_tx,
                        ui_cache.scroll_mode,
                        rect.height(),
                    )
                }
            })
            .inner;
        ui_cache.right_rows = ui
            .scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
                if should_show_editor(app, core::ActivePanel::Right) {
                    let is_focused = app.active_panel == core::ActivePanel::Right;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(core::ActivePanel::Right);
                    if let app_state::PanelMode::Edit(ref mut edit) = panel.mode {
                        ui::editor::draw_editor(
                            ui,
                            ui::editor::EditorRender {
                                theme: &theme,
                                is_focused,
                                edit,
                                highlight_cache,
                                highlight_pending,
                                highlight_req_tx,
                                min_height: rect.height(),
                            },
                        );
                    }
                    ui_cache.right_rows
                } else if should_show_preview(app, core::ActivePanel::Right) {
                    let is_focused = app.active_panel == core::ActivePanel::Right;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(core::ActivePanel::Right);
                    if let app_state::PanelMode::Preview(ref mut preview) = panel.mode {
                        ui::preview::draw_preview(
                            ui,
                            ui::preview::PreviewRender {
                                theme: &theme,
                                is_focused,
                                preview,
                                image_cache,
                                image_req_tx,
                                highlight_cache,
                                highlight_pending,
                                highlight_req_tx,
                                min_height: rect.height(),
                            },
                        );
                    }
                    ui_cache.right_rows
                } else if let Some(_help) = app.help_panel(core::ActivePanel::Right) {
                    let is_focused = app.active_panel == core::ActivePanel::Right;
                    let theme = app.theme.clone();
                    let async_status = app.async_status();
                    ui::help::draw_help(ui, &theme, is_focused, rect.height(), &async_status);
                    ui_cache.right_rows
                } else {
                    ui::panel::draw_panel(
                        ui,
                        app,
                        core::ActivePanel::Right,
                        image_cache,
                        image_req_tx,
                        ui_cache.scroll_mode,
                        rect.height(),
                    )
                }
            })
            .inner;
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                rect.min + egui::Vec2::new(panel_width, 0.0),
                egui::Vec2::new(spacing_x, rect.height()),
            ),
            egui::CornerRadius::ZERO,
            color32(app.theme.colors().divider),
        );
    });
    if app.pending_op.is_some() {
        ui::modals::draw_confirmation(ctx, app);
    }
    if let Some(edit) = app.edit_panel_mut()
        && edit.confirm_discard
    {
        ui::modals::draw_discard_modal(ctx, app);
    }
    #[cfg(unix)]
    if app.props_dialog.is_some() {
        ui::props_dialog::draw_props_modal(ctx, app);
    }
    if app.io_in_flight > 0 {
        ui::modals::draw_progress_modal(ctx, app);
    }

    // Animate loading indicators at ~3fps
    let any_loading = app.left_panel.browser().loading
        || app.right_panel.browser().loading
        || app
            .preview_panel()
            .is_some_and(|p| p.loading_since.is_some());
    if any_loading {
        ctx.request_repaint_after(std::time::Duration::from_millis(333));
    }
}

#[cfg(feature = "self-update")]
fn start_install(app: &mut app_state::AppState) {
    let version = match &app.update_status {
        app_state::UpdateStatus::Available(v) => v.clone(),
        _ => return,
    };
    app.update_status = app_state::UpdateStatus::Installing(version.clone());
    let (tx, rx) = mpsc::channel();
    app.update_rx = Some(rx);
    let wake = app.wake.clone();
    thread::spawn(move || {
        let status = match update::check_for_update() {
            Ok(Some(release)) => match update::perform_update(&release) {
                Ok(()) => app_state::UpdateStatus::Installed(version),
                Err(e) => app_state::UpdateStatus::Failed(e.to_string()),
            },
            Ok(None) => app_state::UpdateStatus::UpToDate,
            Err(e) => app_state::UpdateStatus::Failed(e.to_string()),
        };
        let _ = tx.send(status);
        if let Some(ref w) = wake {
            w();
        }
    });
}

#[cfg(not(feature = "self-update"))]
fn start_install(_app: &mut app_state::AppState) {}

#[cfg(feature = "self-update")]
fn run_update() -> anyhow::Result<()> {
    eprintln!("fileman v{} — checking for updates...", env!("CARGO_PKG_VERSION"));
    match update::check_for_update()? {
        Some(release) => {
            eprintln!("New version available: {} ({})", release.version, release.tag);
            update::perform_update(&release)?;
        }
        None => {
            eprintln!("Already up to date.");
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_default_env()
        .filter_module("egui", log::LevelFilter::Warn)
        .filter_module("egui_winit", log::LevelFilter::Warn)
        .init();

    let args = parse_cli_args()?;
    if let Some(replay_path) = args.replay.as_ref() {
        return replay_runner::run_replay(replay_path, args.snapshot);
    }
    if let Some(snapshot_path) = args.snapshot {
        return replay_runner::run_snapshot(&snapshot_path);
    }
    #[cfg(feature = "self-update")]
    if args.update {
        return run_update();
    }

    let event_loop = winit::event_loop::EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy, args.start_dir);
    event_loop
        .run_app(&mut app)
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}
