use anyhow::Result;
use blade_egui::{GuiPainter, ScreenDescriptor};
use blade_graphics::{
    AlphaMode, CommandEncoderDesc, Context, ContextDesc, Extent, FinishOp, InitOp, RenderTarget,
    RenderTargetSet, SurfaceConfig, SurfaceInfo, TextureColor, TextureDesc, TextureFormat,
    TextureSubresources, TextureUsage, TextureViewDesc, ViewDimension,
};
use egui_winit::State as EguiWinitState;
use exif::{Tag, Value};
use once_cell::sync::Lazy;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::mpsc,
    thread,
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Style, Theme as SyntectTheme, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowAttributes, WindowId},
};
use zune_core::{colorspace::ColorSpace, options::DecoderOptions};
use zune_image::codecs::ImageFormat;
use zune_image::image::Image as ZuneImage;

use fileman::app_state::{
    AppState, BrowserState, EditState, PanelMode, PanelState, PendingOp, PreviewState,
    SearchStatus, SearchUiState,
};
use fileman::core::{
    ActivePanel, BrowserMode, ContainerKind, DirBatch, DirEntry, EditLoadRequest, EditLoadResult,
    EntryLocation, ImageLocation, PreviewContent, PreviewRequest, SearchCase, SearchEvent,
    SearchMode, SearchProgress, SearchRequest, container_display_path, container_kind_from_path,
    format_size, is_media_name, is_text_name, read_container_directory_with_progress,
    read_fs_directory,
};
use fileman::theme::{Color, Theme, ThemeColors, ThemeKind};
use fileman::workers::{
    start_dir_size_worker, start_io_worker, start_preview_worker, start_search_worker,
};

const ROW_HEIGHT: f32 = 24.0;
const SIZE_COL_WIDTH: f32 = 84.0;
const SNAPSHOT_WIDTH: u32 = 1280;
const SNAPSHOT_HEIGHT: u32 = 720;
const MAX_IMAGE_TEXTURES: usize = 64;
const MAX_IMAGE_UPLOADS_PER_FRAME: usize = 2;
const MAX_TEXTURE_SIDE: u32 = 1024;

struct UiCache {
    left_rows: usize,
    right_rows: usize,
    scroll_mode: ScrollMode,
    last_left_selected: usize,
    last_right_selected: usize,
    last_active_panel: ActivePanel,
    last_left_dir_token: u64,
    last_right_dir_token: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScrollMode {
    Default,
    ForceActive,
}

impl UiCache {
    fn update_scroll_mode(&mut self, app: &AppState) {
        let left_selected = AppState::browser(&app.left_panel)
            .map(|b| b.selected_index)
            .unwrap_or(0);
        let right_selected = AppState::browser(&app.right_panel)
            .map(|b| b.selected_index)
            .unwrap_or(0);
        let active = app.active_panel.clone();
        let left_dir = AppState::browser(&app.left_panel)
            .map(|b| b.dir_token)
            .unwrap_or(0);
        let right_dir = AppState::browser(&app.right_panel)
            .map(|b| b.dir_token)
            .unwrap_or(0);
        let selection_changed = left_selected != self.last_left_selected
            || right_selected != self.last_right_selected
            || active != self.last_active_panel
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
    image: egui::ColorImage,
}

enum ImageSource {
    Fs(PathBuf),
    Container {
        kind: ContainerKind,
        archive_path: PathBuf,
        inner_path: String,
    },
}

struct HighlightRequest {
    key: String,
    text: String,
    ext: Option<String>,
    theme_kind: ThemeKind,
}

struct HighlightResult {
    key: String,
    job: egui::text::LayoutJob,
}

struct ImageCache {
    textures: HashMap<String, egui::TextureHandle>,
    pending: HashSet<String>,
    order: VecDeque<String>,
}

fn touch_image(cache: &mut ImageCache, key: &str) {
    if let Some(pos) = cache.order.iter().position(|p| p == key) {
        cache.order.remove(pos);
        cache.order.push_back(key.to_string());
    }
}

fn color32(c: Color) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (c.r.clamp(0.0, 1.0) * 255.0) as u8,
        (c.g.clamp(0.0, 1.0) * 255.0) as u8,
        (c.b.clamp(0.0, 1.0) * 255.0) as u8,
        (c.a.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

fn blend_color(base: Color, tint: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color::rgba(
        base.r + (tint.r - base.r) * t,
        base.g + (tint.g - base.g) * t,
        base.b + (tint.b - base.b) * t,
        base.a,
    )
}

fn decode_image_bytes(bytes: &[u8], max_side: u32) -> Option<egui::ColorImage> {
    let image = ZuneImage::read(bytes, DecoderOptions::default()).ok()?;
    let orientation = exif_orientation(&image).unwrap_or(1);
    let (width, height) = image.dimensions();
    let colorspace = image.colorspace();
    let mut frames = image.flatten_to_u8();
    let data = frames.pop()?;
    let rgba = convert_to_rgba(&data, width, height, colorspace)?;
    let (rgba, width, height) = apply_orientation_rgba(rgba, width, height, orientation);
    let (out_w, out_h, out_rgba) = downscale_rgba(&rgba, width, height, max_side);
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [out_w, out_h],
        &out_rgba,
    ))
}

fn exif_orientation(image: &ZuneImage) -> Option<u16> {
    let exif = image.metadata().exif()?;
    for field in exif {
        if field.tag == Tag::Orientation {
            if let Value::Short(values) = &field.value {
                return values.first().copied();
            }
        }
    }
    None
}

fn apply_orientation_rgba(
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    orientation: u16,
) -> (Vec<u8>, usize, usize) {
    match orientation {
        2 => (flip_horizontal(&rgba, width, height), width, height),
        3 => (rotate_180(&rgba, width, height), width, height),
        4 => (flip_vertical(&rgba, width, height), width, height),
        5 => (
            transpose_flip_horizontal(&rgba, width, height),
            height,
            width,
        ),
        6 => (rotate_90_cw(&rgba, width, height), height, width),
        7 => (transpose_flip_vertical(&rgba, width, height), height, width),
        8 => (rotate_90_ccw(&rgba, width, height), height, width),
        _ => (rgba, width, height),
    }
}

fn flip_horizontal(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst = (y * width + (width - 1 - x)) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn flip_vertical(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst = ((height - 1 - y) * width + x) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn rotate_180(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst = ((height - 1 - y) * width + (width - 1 - x)) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn rotate_90_cw(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst_x = height - 1 - y;
            let dst_y = x;
            let dst = (dst_y * height + dst_x) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn rotate_90_ccw(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst_x = y;
            let dst_y = width - 1 - x;
            let dst = (dst_y * height + dst_x) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn transpose_flip_horizontal(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst_x = height - 1 - y;
            let dst_y = width - 1 - x;
            let dst = (dst_y * height + dst_x) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn transpose_flip_vertical(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst_x = y;
            let dst_y = x;
            let dst = (dst_y * height + dst_x) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn convert_to_rgba(
    data: &[u8],
    width: usize,
    height: usize,
    colorspace: ColorSpace,
) -> Option<Vec<u8>> {
    let pixels = width.checked_mul(height)?;
    match colorspace {
        ColorSpace::RGBA => {
            if data.len() == pixels * 4 {
                Some(data.to_vec())
            } else {
                None
            }
        }
        ColorSpace::RGB => {
            if data.len() != pixels * 3 {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(3) {
                out.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            Some(out)
        }
        ColorSpace::BGR => {
            if data.len() != pixels * 3 {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(3) {
                out.extend_from_slice(&[chunk[2], chunk[1], chunk[0], 255]);
            }
            Some(out)
        }
        ColorSpace::BGRA => {
            if data.len() != pixels * 4 {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(4) {
                out.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
            }
            Some(out)
        }
        ColorSpace::ARGB => {
            if data.len() != pixels * 4 {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(4) {
                out.extend_from_slice(&[chunk[1], chunk[2], chunk[3], chunk[0]]);
            }
            Some(out)
        }
        ColorSpace::Luma => {
            if data.len() != pixels {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for &v in data {
                out.extend_from_slice(&[v, v, v, 255]);
            }
            Some(out)
        }
        ColorSpace::LumaA => {
            if data.len() != pixels * 2 {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(2) {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            Some(out)
        }
        _ => None,
    }
}

fn downscale_rgba(
    rgba: &[u8],
    width: usize,
    height: usize,
    max_side: u32,
) -> (usize, usize, Vec<u8>) {
    let max_dim = width.max(height);
    if max_dim <= max_side as usize {
        return (width, height, rgba.to_vec());
    }
    let scale = max_side as f32 / max_dim as f32;
    let out_w = (width as f32 * scale).round().max(1.0) as usize;
    let out_h = (height as f32 * scale).round().max(1.0) as usize;
    let mut out = vec![0u8; out_w * out_h * 4];
    for y in 0..out_h {
        let src_y = y * height / out_h;
        for x in 0..out_w {
            let src_x = x * width / out_w;
            let src_idx = (src_y * width + src_x) * 4;
            let dst_idx = (y * out_w + x) * 4;
            out[dst_idx..dst_idx + 4].copy_from_slice(&rgba[src_idx..src_idx + 4]);
        }
    }
    (out_w, out_h, out)
}

static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);

fn apply_theme(ctx: &egui::Context, colors: &ThemeColors) {
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

fn pick_theme(theme_kind: ThemeKind) -> &'static SyntectTheme {
    let themes = &THEME_SET.themes;
    let key = match theme_kind {
        ThemeKind::Dark => "base16-ocean.dark",
        ThemeKind::Light => "InspiredGitHub",
    };
    themes
        .get(key)
        .or_else(|| themes.values().next())
        .expect("syntect theme")
}

fn highlight_text_job(
    text: &str,
    extension: Option<&str>,
    theme_kind: ThemeKind,
) -> egui::text::LayoutJob {
    let ext = extension.map(|ext| ext.to_ascii_lowercase());
    if ext.as_deref() == Some("toml") {
        return fileman::syntax::toml::highlight_toml_job(text, theme_kind);
    }
    if ext.as_deref() == Some("nix") {
        return fileman::syntax::nix::highlight_nix_job(text, theme_kind);
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
    let mut highlighter = HighlightLines::new(syntax, pick_theme(theme_kind));
    let mut job = egui::text::LayoutJob::default();
    for line in LinesWithEndings::from(text) {
        let ranges = highlighter
            .highlight_line(line, &SYNTAX_SET)
            .unwrap_or_else(|_| vec![(Style::default(), line)]);
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

fn panel_path_display(panel: &PanelState) -> String {
    let Some(browser) = AppState::browser(panel) else {
        return String::new();
    };
    match &browser.browser_mode {
        BrowserMode::Fs => browser.current_path.to_string_lossy().into_owned(),
        BrowserMode::Container {
            kind,
            archive_path,
            cwd,
        } => container_display_path(*kind, archive_path, cwd),
        BrowserMode::Search {
            root,
            query,
            mode,
            case,
        } => {
            let mode_label = match mode {
                SearchMode::Name => "name",
                SearchMode::Content => "content",
            };
            let case_label = match case {
                SearchCase::Sensitive => "Aa",
                SearchCase::Insensitive => "aA",
            };
            format!(
                "Search ({mode_label}/{case_label}): \"{query}\" in {}",
                root.to_string_lossy()
            )
        }
    }
}

fn apply_dir_batch(browser: &mut BrowserState, batch: DirBatch) {
    let prior_selection = browser
        .entries
        .get(browser.selected_index)
        .map(|e| e.name.clone());

    match batch {
        DirBatch::Loading => {
            browser.loading = true;
            browser.loading_progress = None;
            return;
        }
        DirBatch::Error(message) => {
            browser.entries = vec![DirEntry {
                name: message,
                is_dir: false,
                location: EntryLocation::Fs(browser.current_path.clone()),
                size: None,
            }];
            browser.selected_index = 0;
            browser.top_index = 0;
            browser.loading = false;
            browser.loading_progress = None;
            return;
        }
        DirBatch::Progress { loaded, total } => {
            browser.loading_progress = Some((loaded, total));
            browser.loading = total.map(|t| loaded < t).unwrap_or(true);
            return;
        }
        DirBatch::Append(mut new_entries) => {
            browser.entries.append(&mut new_entries);
            browser.loading = false;
        }
        DirBatch::Replace(new_entries) => {
            browser.entries = new_entries;
            browser.selected_index = 0;
            browser.top_index = 0;
            browser.loading = false;
        }
    }

    let restore_name = browser.prefer_select_name.take().or(prior_selection);
    if let Some(pref) = restore_name
        && let Some(idx) = browser.entries.iter().position(|e| e.name == pref)
    {
        browser.selected_index = idx;
    }
}

fn pump_async(app: &mut AppState) -> bool {
    let mut changed = false;
    for side in [ActivePanel::Left, ActivePanel::Right] {
        let panel = app.panel_mut(side.clone());
        let Some(browser) = AppState::browser_mut(panel) else {
            continue;
        };
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

    match app.preview_rx.try_recv() {
        Ok((id, content)) => {
            if let Some(preview) = app.preview_panel_mut() {
                if id == preview.request_id {
                    preview.content = Some(content);
                    changed = true;
                }
            }
        }
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => {}
    }

    while let Ok((path, size)) = app.dir_size_rx.try_recv() {
        app.dir_size_pending.remove(&path);
        app.dir_sizes.insert(path.clone(), size);
        for side in [ActivePanel::Left, ActivePanel::Right] {
            let panel = app.panel_mut(side.clone());
            if let Some(browser) = AppState::browser_mut(panel) {
                for entry in &mut browser.entries {
                    if entry.is_dir {
                        if let EntryLocation::Fs(p) = &entry.location {
                            if *p == path {
                                entry.size = Some(size);
                            }
                        }
                    }
                }
            }
        }
        changed = true;
    }

    while let Ok(result) = app.edit_rx.try_recv() {
        if let Some(edit) = app.edit_panel_mut() {
            if result.id == edit.request_id {
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
    }

    while let Ok(event) = app.search_rx.try_recv() {
        match event {
            SearchEvent::Match { id, result } => {
                if id == app.search_request_id {
                    app.search_results.push(result);
                    let result = app.search_results.last().unwrap().clone();
                    let progress_for_panel = match app.search_status {
                        SearchStatus::Running(mut progress) => {
                            progress.matched = progress.matched.saturating_add(1);
                            app.search_status = SearchStatus::Running(progress);
                            Some((progress.matched, None))
                        }
                        SearchStatus::Done(mut progress) => {
                            progress.matched = progress.matched.saturating_add(1);
                            app.search_status = SearchStatus::Done(progress);
                            Some((progress.matched, None))
                        }
                        SearchStatus::Idle => None,
                    };
                    let panel = app.get_active_panel_mut();
                    let Some(browser) = AppState::browser_mut(panel) else {
                        continue;
                    };
                    let display_name = match &browser.browser_mode {
                        BrowserMode::Search { root, .. } => result
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
                    browser.entries.push(DirEntry {
                        name: display_name,
                        is_dir: result.is_dir,
                        location: EntryLocation::Fs(result.path),
                        size: result.size,
                    });
                    if let Some(progress) = progress_for_panel {
                        browser.loading_progress = Some(progress);
                    }
                    changed = true;
                }
            }
            SearchEvent::Progress { id, progress } => {
                if id == app.search_request_id {
                    app.search_status = SearchStatus::Running(progress);
                    let panel = app.get_active_panel_mut();
                    if let Some(browser) = AppState::browser_mut(panel) {
                        browser.loading_progress = Some((progress.matched, Some(progress.scanned)));
                    }
                    changed = true;
                }
            }
            SearchEvent::Done { id, progress } => {
                if id == app.search_request_id {
                    app.search_status = SearchStatus::Done(progress);
                    let panel = app.get_active_panel_mut();
                    if let Some(browser) = AppState::browser_mut(panel) {
                        browser.loading = false;
                        browser.loading_progress = Some((progress.matched, Some(progress.scanned)));
                    }
                    changed = true;
                }
            }
            SearchEvent::Error { id, message } => {
                if id == app.search_request_id {
                    eprintln!("Search error: {message}");
                    app.search_status = SearchStatus::Done(SearchProgress {
                        scanned: 0,
                        matched: 0,
                    });
                    let panel = app.get_active_panel_mut();
                    if let Some(browser) = AppState::browser_mut(panel) {
                        browser.loading = false;
                    }
                    changed = true;
                }
            }
        }
    }

    changed
}

fn load_fs_directory_async(
    app: &mut AppState,
    path: PathBuf,
    target_panel: ActivePanel,
    prefer_name: Option<String>,
) {
    let mut initial: Vec<DirEntry> = Vec::new();
    let mut has_parent_entry = false;
    if path.parent().is_some() {
        initial.push(DirEntry {
            name: "..".to_string(),
            is_dir: true,
            location: EntryLocation::Fs(path.parent().unwrap().to_path_buf()),
            size: None,
        });
        has_parent_entry = true;
    }

    let (tx, rx) = mpsc::channel::<DirBatch>();
    let path_clone = path.clone();
    let dir_sizes_snapshot = app.dir_sizes.clone();
    let dir_sizes_fallback = app.dir_sizes.clone();

    if let Ok(mut rd) = fs::read_dir(&path) {
        let mut snapshot: Vec<DirEntry> = Vec::with_capacity(128);
        for _ in 0..128 {
            match rd.next() {
                Some(Ok(entry)) => {
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                    let size = if is_dir {
                        dir_sizes_snapshot.get(&entry.path()).copied()
                    } else {
                        entry.metadata().ok().map(|m| m.len())
                    };
                    snapshot.push(DirEntry {
                        name: file_name,
                        is_dir,
                        location: EntryLocation::Fs(entry.path()),
                        size,
                    });
                }
                Some(Err(_)) | None => break,
            }
        }
        if !snapshot.is_empty() {
            let _ = tx.send(DirBatch::Append(snapshot.clone()));
        }
        let mut snapshot = snapshot;
        thread::spawn(move || {
            let chunk = 500usize;
            let mut all: Vec<DirEntry> = Vec::new();
            all.append(&mut snapshot);
            for entry in rd.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();
                if let Ok(file_type) = entry.file_type() {
                    let is_dir = file_type.is_dir();
                    let size = if is_dir {
                        dir_sizes_snapshot.get(&entry.path()).copied()
                    } else {
                        entry.metadata().ok().map(|m| m.len())
                    };
                    all.push(DirEntry {
                        name: file_name,
                        is_dir,
                        location: EntryLocation::Fs(entry.path()),
                        size,
                    });
                }
            }
            all.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
            let mut sorted: Vec<DirEntry> = Vec::new();
            if let Some(parent) = path_clone.parent() {
                sorted.push(DirEntry {
                    name: "..".to_string(),
                    is_dir: true,
                    location: EntryLocation::Fs(parent.to_path_buf()),
                    size: None,
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
                    let _ = tx.send(DirBatch::Replace(batch));
                } else {
                    let _ = tx.send(DirBatch::Append(batch));
                }
                start = end;
            }
        });
    } else {
        thread::spawn(move || {
            let chunk = 500usize;
            let mut all: Vec<DirEntry> = Vec::new();
            if let Ok(read_dir) = fs::read_dir(&path_clone) {
                for entry in read_dir.flatten() {
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    if let Ok(file_type) = entry.file_type() {
                        let is_dir = file_type.is_dir();
                        let size = if is_dir {
                            dir_sizes_fallback.get(&entry.path()).copied()
                        } else {
                            entry.metadata().ok().map(|m| m.len())
                        };
                        all.push(DirEntry {
                            name: file_name,
                            is_dir,
                            location: EntryLocation::Fs(entry.path()),
                            size,
                        });
                    }
                }
            }
            all.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
            let mut sorted: Vec<DirEntry> = Vec::new();
            if let Some(parent) = path_clone.parent() {
                sorted.push(DirEntry {
                    name: "..".to_string(),
                    is_dir: true,
                    location: EntryLocation::Fs(parent.to_path_buf()),
                    size: None,
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
                    let _ = tx.send(DirBatch::Replace(batch));
                } else {
                    let _ = tx.send(DirBatch::Append(batch));
                }
                start = end;
            }
        });
    }

    let remembered = prefer_name
        .clone()
        .or_else(|| app.fs_last_selected_name.get(&path).cloned());
    let panel_state = app.panel_mut(target_panel);
    let Some(browser) = AppState::browser_mut(panel_state) else {
        return;
    };
    let initial_loading = initial.is_empty() || has_parent_entry;
    browser.current_path = path.clone();
    browser.browser_mode = BrowserMode::Fs;
    browser.entries = initial;
    browser.selected_index = 0;
    browser.top_index = 0;
    browser.dir_token = browser.dir_token.wrapping_add(1);
    browser.entries_rx = Some(rx);
    browser.prefer_select_name = remembered;
    browser.loading = initial_loading;
    browser.loading_progress = None;
}

fn load_container_directory_async(
    app: &mut AppState,
    kind: ContainerKind,
    archive_path: PathBuf,
    cwd: String,
    target_panel: ActivePanel,
    prefer_name: Option<String>,
) {
    let mut initial: Vec<DirEntry> = Vec::new();
    if !cwd.is_empty() {
        let parent = cwd
            .trim_end_matches('/')
            .rsplit_once('/')
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
        initial.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            location: EntryLocation::Container {
                kind,
                archive_path: archive_path.clone(),
                inner_path: parent,
            },
            size: None,
        });
    } else {
        let parent = archive_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        initial.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            location: EntryLocation::Fs(parent),
            size: None,
        });
    }

    let (tx, rx) = mpsc::channel::<DirBatch>();
    let archive_clone = archive_path.clone();
    let cwd_clone = cwd.clone();
    let kind_clone = kind;

    if kind == ContainerKind::TarBz2 {
        thread::spawn(move || {
            let prefix = if cwd_clone.is_empty() {
                "".to_string()
            } else {
                format!("{}/", cwd_clone.trim_end_matches('/'))
            };
            let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut seen_files: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut pending: Vec<DirEntry> = Vec::new();
            let mut loaded = 0usize;
            let mut sent_first = false;
            const BATCH: usize = 200;
            const DECIDE_LIMIT: usize = 64;
            let mut decided = false;
            let mut implicit_root: Option<String> = None;
            let mut buffered: Vec<String> = Vec::new();
            let mut root_candidate: Option<String> = None;
            let mut seen_root_file = false;
            let mut seen_other_root = false;

            fn emit_name(
                name: &str,
                implicit_prefix: Option<&str>,
                cwd: &str,
                kind: ContainerKind,
                archive_path: &PathBuf,
                pending: &mut Vec<DirEntry>,
                seen_dirs: &mut std::collections::HashSet<String>,
                seen_files: &mut std::collections::HashSet<String>,
                loaded: &mut usize,
            ) {
                let rem = if let Some(prefix) = implicit_prefix {
                    if !name.starts_with(prefix) {
                        return;
                    }
                    let trimmed = name[prefix.len()..].trim_start_matches('/');
                    if trimmed.is_empty() {
                        return;
                    }
                    trimmed
                } else {
                    name
                };

                if let Some(slash) = rem.find('/') {
                    let dir = rem[..slash].to_string();
                    if seen_dirs.insert(dir.clone()) {
                        pending.push(DirEntry {
                            name: dir.clone(),
                            is_dir: true,
                            location: EntryLocation::Container {
                                kind,
                                archive_path: archive_path.clone(),
                                inner_path: if let Some(prefix) = implicit_prefix {
                                    format!("{}{}", prefix, dir)
                                } else if cwd.is_empty() {
                                    dir
                                } else {
                                    format!("{}/{}", cwd.trim_end_matches('/'), dir)
                                },
                            },
                            size: None,
                        });
                        *loaded += 1;
                    }
                } else if seen_files.insert(rem.to_string()) {
                    let file_name = rem.to_string();
                    pending.push(DirEntry {
                        name: file_name.clone(),
                        is_dir: false,
                        location: EntryLocation::Container {
                            kind,
                            archive_path: archive_path.clone(),
                            inner_path: if let Some(prefix) = implicit_prefix {
                                format!("{}{}", prefix, file_name)
                            } else if cwd.is_empty() {
                                file_name
                            } else {
                                format!("{}/{}", cwd.trim_end_matches('/'), file_name)
                            },
                        },
                        size: None,
                    });
                    *loaded += 1;
                }
            }

            let file = match std::fs::File::open(&archive_clone) {
                Ok(file) => file,
                Err(e) => {
                    let _ = tx.send(DirBatch::Error(format!("Failed to read archive: {e}")));
                    return;
                }
            };
            let reader = std::io::BufReader::new(file);
            let decoder = bzip2::read::BzDecoder::new(reader);
            let mut archive = tar::Archive::new(decoder);
            let entries = match archive.entries() {
                Ok(entries) => entries,
                Err(e) => {
                    let _ = tx.send(DirBatch::Error(format!("Failed to read archive: {e}")));
                    return;
                }
            };

            for entry in entries.flatten() {
                let path = match entry.path() {
                    Ok(path) => path,
                    Err(_) => continue,
                };
                let name = fileman::core::normalize_archive_path(&path);
                if name.is_empty() || !name.starts_with(&prefix) {
                    continue;
                }
                let rem = &name[prefix.len()..];
                if rem.is_empty() {
                    continue;
                }
                if !decided && cwd_clone.is_empty() {
                    buffered.push(name.clone());
                    if let Some(slash) = rem.find('/') {
                        let root = rem[..slash].to_string();
                        match root_candidate.as_ref() {
                            None => root_candidate = Some(root),
                            Some(existing) if existing != &root => seen_other_root = true,
                            _ => {}
                        }
                    } else {
                        seen_root_file = true;
                    }

                    if buffered.len() >= DECIDE_LIMIT || seen_root_file || seen_other_root {
                        decided = true;
                        if !seen_root_file && !seen_other_root {
                            implicit_root = root_candidate.clone();
                        }
                        let root_ref = implicit_root
                            .as_ref()
                            .map(|root| format!("{}/", root.trim_end_matches('/')));
                        for buffered_name in buffered.drain(..) {
                            emit_name(
                                &buffered_name,
                                root_ref.as_deref(),
                                &cwd_clone,
                                kind_clone,
                                &archive_clone,
                                &mut pending,
                                &mut seen_dirs,
                                &mut seen_files,
                                &mut loaded,
                            );
                        }
                    } else {
                        continue;
                    }
                } else {
                    emit_name(
                        &name,
                        implicit_root
                            .as_ref()
                            .map(|root| format!("{}/", root.trim_end_matches('/')))
                            .as_deref(),
                        &cwd_clone,
                        kind_clone,
                        &archive_clone,
                        &mut pending,
                        &mut seen_dirs,
                        &mut seen_files,
                        &mut loaded,
                    );
                }

                if pending.len() >= BATCH || (!sent_first && !pending.is_empty()) {
                    let _ = tx.send(DirBatch::Append(pending));
                    pending = Vec::new();
                    sent_first = true;
                    let _ = tx.send(DirBatch::Progress {
                        loaded,
                        total: None,
                    });
                }
            }

            if !decided && cwd_clone.is_empty() {
                if !seen_root_file && !seen_other_root {
                    implicit_root = root_candidate.clone();
                }
                let root_ref = implicit_root
                    .as_ref()
                    .map(|root| format!("{}/", root.trim_end_matches('/')));
                for buffered_name in buffered.drain(..) {
                    emit_name(
                        &buffered_name,
                        root_ref.as_deref(),
                        &cwd_clone,
                        kind_clone,
                        &archive_clone,
                        &mut pending,
                        &mut seen_dirs,
                        &mut seen_files,
                        &mut loaded,
                    );
                }
            }

            if !pending.is_empty() {
                let _ = tx.send(DirBatch::Append(pending));
            }
            let _ = tx.send(DirBatch::Progress {
                loaded,
                total: Some(loaded),
            });
        });
    } else {
        thread::spawn(move || {
            let all = match read_container_directory_with_progress(
                kind_clone,
                &archive_clone,
                &cwd_clone,
                |loaded| {
                    let _ = tx.send(DirBatch::Progress {
                        loaded,
                        total: None,
                    });
                },
            ) {
                Ok(entries) => entries,
                Err(e) => {
                    eprintln!("Failed to read container: {e}");
                    let _ = tx.send(DirBatch::Error(format!("Failed to read archive: {e}")));
                    return;
                }
            };
            let total = all.len();
            let initial = all.iter().take(128).cloned().collect::<Vec<_>>();
            let loaded = initial.len().min(total);
            if !initial.is_empty() {
                let _ = tx.send(DirBatch::Replace(initial));
                let _ = tx.send(DirBatch::Progress {
                    loaded,
                    total: Some(total),
                });
            }
            thread::spawn(move || {
                let chunk = 500usize;
                let mut start = 128.min(all.len());
                while start < all.len() {
                    let end = (start + chunk).min(all.len());
                    let _ = tx.send(DirBatch::Append(all[start..end].to_vec()));
                    let _ = tx.send(DirBatch::Progress {
                        loaded: end,
                        total: Some(total),
                    });
                    start = end;
                }
            });
        });
    }

    let remembered = prefer_name.clone().or_else(|| {
        app.container_last_selected_name
            .get(&(archive_path.clone(), cwd.clone(), kind))
            .cloned()
    });
    let panel_state = app.panel_mut(target_panel);
    let Some(browser) = AppState::browser_mut(panel_state) else {
        return;
    };
    let initial_loading = true;

    browser.current_path = archive_path.clone();
    browser.browser_mode = BrowserMode::Container {
        kind,
        archive_path: archive_path.clone(),
        cwd: cwd.clone(),
    };
    browser.entries = initial;
    browser.selected_index = 0;
    browser.top_index = 0;
    browser.dir_token = browser.dir_token.wrapping_add(1);
    browser.entries_rx = Some(rx);
    browser.prefer_select_name = remembered;
    browser.loading = initial_loading;
    browser.loading_progress = None;
}

fn open_selected(app: &mut AppState) {
    let active = app.active_panel.clone();

    open_selected_from_to(app, active.clone(), active);
}

fn should_show_preview(app: &AppState, panel_side: ActivePanel) -> bool {
    matches!(app.panel(panel_side).mode, PanelMode::Preview(_))
}

fn should_show_editor(app: &AppState, panel_side: ActivePanel) -> bool {
    matches!(app.panel(panel_side).mode, PanelMode::Edit(_))
}

fn open_selected_from_to(app: &mut AppState, source: ActivePanel, target: ActivePanel) {
    let (selected_entry, current_path, container_cwd) = {
        let panel = app.panel(source.clone());
        let Some(browser) = AppState::browser(panel) else {
            return;
        };
        if browser.entries.is_empty() {
            return;
        }
        let entry = browser.entries[browser.selected_index].clone();
        let current_path = browser.current_path.clone();
        let container_cwd = match &browser.browser_mode {
            BrowserMode::Container { cwd, .. } => Some(cwd.clone()),
            _ => None,
        };
        (entry, current_path, container_cwd)
    };

    app.store_selection_memory_for(source);
    app.push_history(target.clone());

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
                load_fs_directory_async(app, path.clone(), target.clone(), prefer_name);

                if selected_entry.name != ".."
                    && let Some(name) = app.fs_last_selected_name.get(path).cloned()
                {
                    app.select_entry_by_name(target, &name);
                }
            } else if let Some(kind) = container_kind_from_path(path) {
                load_container_directory_async(
                    app,
                    kind,
                    path.clone(),
                    "".to_string(),
                    target,
                    None,
                );
            }
        }
        EntryLocation::Container {
            kind,
            archive_path,
            inner_path,
        } => {
            if selected_entry.is_dir {
                let prefer_name = if selected_entry.name == ".." {
                    container_cwd.as_ref().and_then(|cwd| {
                        cwd.trim_end_matches('/')
                            .rsplit('/')
                            .next()
                            .map(|s| s.to_string())
                    })
                } else {
                    None
                };
                load_container_directory_async(
                    app,
                    *kind,
                    archive_path.clone(),
                    inner_path.clone(),
                    target.clone(),
                    prefer_name,
                );

                if selected_entry.name != ".."
                    && let Some(name) = app
                        .container_last_selected_name
                        .get(&(archive_path.clone(), inner_path.clone(), *kind))
                        .cloned()
                {
                    app.select_entry_by_name(target, &name);
                }
            }
        }
    }

    while let Ok((path, size)) = app.dir_size_rx.try_recv() {
        app.dir_size_pending.remove(&path);
        app.dir_sizes.insert(path.clone(), size);
        for side in [ActivePanel::Left, ActivePanel::Right] {
            let panel = app.panel_mut(side.clone());
            if let Some(browser) = AppState::browser_mut(panel) {
                for entry in &mut browser.entries {
                    if entry.is_dir {
                        if let EntryLocation::Fs(p) = &entry.location {
                            if *p == path {
                                entry.size = Some(size);
                            }
                        }
                    }
                }
            }
        }
    }
}

fn window_rows_for(panel_height: f32, spacing: f32) -> usize {
    let row = ROW_HEIGHT + spacing;
    if panel_height <= 0.0 || row <= 0.0 {
        return 10;
    }
    ((panel_height / row).floor() as usize).max(1)
}

fn active_window_rows(app: &AppState, cache: &UiCache) -> usize {
    match app.active_panel {
        ActivePanel::Left => cache.left_rows,
        ActivePanel::Right => cache.right_rows,
    }
}

fn open_search(app: &mut AppState, mode: SearchMode) {
    app.search_ui = SearchUiState::Open;
    app.search_focus = true;
    app.search_mode = mode;
}

fn preview_find_next(app: &mut AppState) {
    let Some(preview) = app.preview_panel_mut() else {
        return;
    };
    let Some(PreviewContent::Text(text)) = preview.content.as_ref() else {
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
    app: &mut AppState,
    which: ActivePanel,
    snapshot: fileman::app_state::PanelSnapshot,
) {
    match snapshot.mode {
        BrowserMode::Fs => {
            load_fs_directory_async(app, snapshot.current_path, which, snapshot.selected_name);
        }
        BrowserMode::Container {
            kind,
            archive_path,
            cwd,
        } => {
            load_container_directory_async(
                app,
                kind,
                archive_path,
                cwd,
                which,
                snapshot.selected_name,
            );
        }
        BrowserMode::Search { .. } => {
            let results = app.search_results.clone();
            let panel = app.panel_mut(which);
            let Some(browser) = AppState::browser_mut(panel) else {
                return;
            };
            browser.browser_mode = snapshot.mode;
            browser.current_path = snapshot.current_path;
            browser.entries.clear();
            browser.entries.extend(results.iter().map(|result| {
                let display_name = match &browser.browser_mode {
                    BrowserMode::Search { root, .. } => result
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
                DirEntry {
                    name: display_name,
                    is_dir: result.is_dir,
                    location: EntryLocation::Fs(result.path.clone()),
                    size: result.size,
                }
            }));
            browser.entries_rx = None;
            browser.selected_index = snapshot
                .selected_name
                .and_then(|name| {
                    if let Some(path) = name.strip_prefix("fs:") {
                        return browser.entries.iter().position(|e| {
                            if let EntryLocation::Fs(p) = &e.location {
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

fn cancel_search(app: &mut AppState) {
    app.search_request_id = app.search_request_id.wrapping_add(1);
    app.search_status = SearchStatus::Idle;
}

fn start_search(app: &mut AppState) {
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
    app.search_status = SearchStatus::Running(SearchProgress {
        scanned: 0,
        matched: 0,
    });
    let root = {
        let panel = app.get_active_panel();
        let Some(browser) = AppState::browser(panel) else {
            return;
        };
        browser.current_path.clone()
    };
    {
        app.push_history(app.active_panel.clone());
        let panel = app.get_active_panel_mut();
        let browser = &mut panel.browser;
        browser.current_path = root.clone();
        browser.browser_mode = BrowserMode::Search {
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
        panel.mode = PanelMode::Browser;
    }
    let _ = app.search_tx.send(SearchRequest {
        id,
        root,
        needle,
        case: search_case,
        mode: search_mode,
    });
}

fn handle_keyboard(
    ctx: &egui::Context,
    input: &egui::InputState,
    app: &mut AppState,
    cache: &mut UiCache,
) {
    let io_tx = app.io_tx.clone();
    if app.io_in_flight > 0 && input.key_pressed(egui::Key::Escape) {
        app.request_io_cancel();
        ctx.request_repaint();
        return;
    }
    if app.pending_op.is_some() {
        if input.key_pressed(egui::Key::Enter) {
            confirm_pending_op(app);
        }
        if input.key_pressed(egui::Key::Escape) {
            app.clear_pending_op();
        }
        ctx.request_repaint();
        return;
    }
    if let PanelMode::Edit(edit) = &mut app.panel_mut(app.active_panel.clone()).mode {
        let enter = input.key_pressed(egui::Key::Enter);
        let escape = input.key_pressed(egui::Key::Escape);
        let ctrl_s = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::S));
        let mut refresh_after = false;
        let mut save_payload: Option<(PathBuf, Vec<u8>)> = None;
        let mut close_editor = false;
        if edit.confirm_discard {
            if enter {
                close_editor = true;
            } else if escape {
                edit.confirm_discard = false;
            }
            ctx.request_repaint();
            if close_editor {
                let return_focus = edit.return_focus.clone();
                let panel = app.panel_mut(app.active_panel.clone());
                panel.mode = PanelMode::Browser;
                app.active_panel = return_focus;
            }
            return;
        }
        if !input.events.is_empty() {
            ctx.request_repaint();
        }
        if ctrl_s {
            if let Some(path) = edit.path.clone() {
                save_payload = Some((path, edit.text.as_bytes().to_vec()));
                edit.dirty = false;
                edit.confirm_discard = false;
                refresh_after = true;
                close_editor = true;
            }
            ctx.request_repaint();
            if let Some((path, contents)) = save_payload {
                let _ = io_tx.send(fileman::core::IOTask::WriteFile { path, contents });
            }
            if close_editor {
                let return_focus = edit.return_focus.clone();
                let panel = app.panel_mut(app.active_panel.clone());
                panel.mode = PanelMode::Browser;
                app.active_panel = return_focus;
            }
            if refresh_after {
                refresh_active_panel(app);
            }
            return;
        }
        if escape {
            if edit.dirty {
                edit.confirm_discard = true;
            } else {
                close_editor = true;
            }
            ctx.request_repaint();
            if close_editor {
                let return_focus = edit.return_focus.clone();
                let panel = app.panel_mut(app.active_panel.clone());
                panel.mode = PanelMode::Browser;
                app.active_panel = return_focus;
            }
            return;
        }
        return;
    }
    if app.search_ui == SearchUiState::Open {
        if input.key_pressed(egui::Key::Escape) {
            cancel_search(app);
            app.search_ui = SearchUiState::Closed;
            ctx.request_repaint();
            return;
        }
        let ctrl_enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter));
        if ctrl_enter {
            start_search(app);
            ctx.request_repaint();
        }
    }
    let window_rows = active_window_rows(app, cache);
    let tab_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
    let ctrl_tab_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::I));
    if tab_pressed || ctrl_tab_pressed {
        app.switch_panel();
        ctx.request_repaint();
    }
    let ctrl_pgup = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::PageUp));
    let backspace = input.key_pressed(egui::Key::Backspace);
    let typing_in_ui = ctx.wants_keyboard_input();
    if (ctrl_pgup || backspace) && !(app.search_ui == SearchUiState::Open && typing_in_ui) {
        open_parent(app, window_rows);
    }
    let ctrl_pgdn = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::PageDown));
    if ctrl_pgdn {
        open_selected(app);
    }
    let ctrl_r = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::R));
    if ctrl_r {
        refresh_active_panel(app);
    }
    let space = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space));
    if space {
        let target_path = {
            let panel = app.get_active_panel();
            AppState::browser(panel).and_then(|browser| {
                browser
                    .entries
                    .get(browser.selected_index)
                    .and_then(|entry| {
                        if entry.is_dir {
                            if let EntryLocation::Fs(path) = &entry.location {
                                return Some(path.clone());
                            }
                        }
                        None
                    })
            })
        };
        if let Some(path) = target_path {
            if !app.dir_size_pending.contains(&path) {
                app.dir_size_pending.insert(path.clone());
                let _ = app.dir_size_tx.send(path);
            }
        }
    }
    let ctrl_left = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowLeft));
    if ctrl_left && app.active_panel == ActivePanel::Right {
        open_selected_from_to(app, ActivePanel::Right, ActivePanel::Left);
    }
    let ctrl_right = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowRight));
    if ctrl_right && app.active_panel == ActivePanel::Left {
        open_selected_from_to(app, ActivePanel::Left, ActivePanel::Right);
    }
    let alt_left = ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft));
    if alt_left {
        if let Some(snapshot) = app.pop_history_back(app.active_panel.clone()) {
            apply_panel_snapshot(app, app.active_panel.clone(), snapshot);
        }
    }
    let alt_right = ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight));
    if alt_right {
        if let Some(snapshot) = app.pop_history_forward(app.active_panel.clone()) {
            apply_panel_snapshot(app, app.active_panel.clone(), snapshot);
        }
    }
    let alt_f7 = ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::F7));
    if alt_f7 {
        open_search(app, SearchMode::Name);
    }
    let shift_alt_f7 = ctx
        .input_mut(|i| i.consume_key(egui::Modifiers::ALT | egui::Modifiers::SHIFT, egui::Key::F7));
    if shift_alt_f7 {
        open_search(app, SearchMode::Content);
    }
    if input.key_pressed(egui::Key::Enter) {
        if app.search_ui == SearchUiState::Open {
            if AppState::browser(app.get_active_panel())
                .is_some_and(|browser| matches!(browser.browser_mode, BrowserMode::Search { .. }))
            {
                // Open selected result.
            } else {
                start_search(app);
            }
        }
        if AppState::browser(app.get_active_panel())
            .is_some_and(|browser| matches!(browser.browser_mode, BrowserMode::Search { .. }))
        {
            app.push_history(app.active_panel.clone());
            let entry = AppState::browser(app.get_active_panel())
                .and_then(|browser| browser.entries.get(browser.selected_index).cloned());
            if let Some(entry) = entry {
                if let EntryLocation::Fs(path) = entry.location {
                    if entry.is_dir {
                        load_fs_directory_async(app, path, app.active_panel.clone(), None);
                    } else if let Some(parent) = path.parent() {
                        let name = path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_string());
                        load_fs_directory_async(
                            app,
                            parent.to_path_buf(),
                            app.active_panel.clone(),
                            name,
                        );
                    }
                }
            }
            app.search_ui = SearchUiState::Closed;
        } else if app.theme_picker_open {
            app.apply_selected_theme();
        } else {
            open_selected(app);
        }
    }
    if let PanelMode::Preview(preview) = &mut app.panel_mut(app.active_panel.clone()).mode {
        let line = preview.line_height.max(16.0);
        let page = preview.page_height.max(200.0);
        let mut consumed = false;
        let can_scroll = preview.can_scroll;
        if can_scroll && input.key_pressed(egui::Key::ArrowDown) {
            preview.scroll += line;
            consumed = true;
        }
        if can_scroll && input.key_pressed(egui::Key::ArrowUp) {
            preview.scroll = (preview.scroll - line).max(0.0);
            consumed = true;
        }
        if can_scroll && input.key_pressed(egui::Key::PageDown) {
            preview.scroll += page;
            consumed = true;
        }
        if can_scroll && input.key_pressed(egui::Key::PageUp) {
            preview.scroll = (preview.scroll - page).max(0.0);
            consumed = true;
        }
        if can_scroll && input.key_pressed(egui::Key::Home) {
            preview.scroll = 0.0;
            consumed = true;
        }
        if can_scroll && input.key_pressed(egui::Key::End) {
            preview.scroll = preview.scroll + page * 10.0;
            consumed = true;
        }
        let enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        if enter && preview.find_open {
            preview_find_next(app);
            consumed = true;
        }
        if consumed {
            ctx.request_repaint();
            return;
        }
    }
    let ctrl_f = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::F));
    if ctrl_f {
        if let PanelMode::Preview(preview) = &mut app.panel_mut(app.active_panel.clone()).mode {
            preview.find_open = true;
            preview.find_focus = true;
        }
        ctx.request_repaint();
    }
    if input.key_pressed(egui::Key::ArrowDown)
        && matches!(app.panel(app.active_panel.clone()).mode, PanelMode::Browser)
    {
        if app.theme_picker_open {
            app.select_next_theme();
        } else if let Some(browser) = AppState::browser(app.get_active_panel()) {
            if browser.selected_index + 1 < browser.entries.len() {
                app.select_entry(browser.selected_index + 1, window_rows);
            }
        }
    }
    if input.key_pressed(egui::Key::ArrowUp)
        && matches!(app.panel(app.active_panel.clone()).mode, PanelMode::Browser)
    {
        if app.theme_picker_open {
            app.select_prev_theme();
        } else if let Some(browser) = AppState::browser(app.get_active_panel()) {
            if browser.selected_index > 0 {
                app.select_entry(browser.selected_index - 1, window_rows);
            }
        }
    }
    if input.key_pressed(egui::Key::PageUp)
        && matches!(app.panel(app.active_panel.clone()).mode, PanelMode::Browser)
    {
        let Some(browser) = AppState::browser(app.get_active_panel()) else {
            return;
        };
        let new_index = browser.selected_index.saturating_sub(window_rows);
        app.select_entry(new_index, window_rows);
    }
    if input.key_pressed(egui::Key::PageDown)
        && matches!(app.panel(app.active_panel.clone()).mode, PanelMode::Browser)
    {
        let Some(browser) = AppState::browser(app.get_active_panel()) else {
            return;
        };
        let len = browser.entries.len();
        let mut new_index = browser.selected_index.saturating_add(window_rows);
        if len > 0 && new_index >= len {
            new_index = len - 1;
        }
        app.select_entry(new_index, window_rows);
    }
    if input.key_pressed(egui::Key::Home)
        && matches!(app.panel(app.active_panel.clone()).mode, PanelMode::Browser)
    {
        app.select_entry(0, window_rows);
    }
    if input.key_pressed(egui::Key::End)
        && matches!(app.panel(app.active_panel.clone()).mode, PanelMode::Browser)
    {
        if let Some(browser) = AppState::browser(app.get_active_panel()) {
            if !browser.entries.is_empty() {
                app.select_entry(browser.entries.len() - 1, window_rows);
            }
        }
    }
    if input.key_pressed(egui::Key::F3) {
        app.toggle_preview();
    }
    if input.key_pressed(egui::Key::Escape) {
        if app.theme_picker_open {
            app.close_theme_picker();
        } else {
            app.clear_preview();
        }
    }
    if input.key_pressed(egui::Key::F5) {
        app.prepare_copy_selected();
    }
    if input.key_pressed(egui::Key::F6) {
        app.prepare_move_selected();
    }
    if input.key_pressed(egui::Key::F4) {
        app.prepare_edit_selected();
        ctx.request_repaint();
    }
    let shift_f6 = ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::F6));
    if shift_f6 {
        app.prepare_rename_selected();
    }
    if input.key_pressed(egui::Key::F9) {
        app.switch_theme();
    }
    if input.key_pressed(egui::Key::F10) {
        app.open_theme_picker();
    }
    if input.key_pressed(egui::Key::F8) {
        app.prepare_delete_selected();
    }
}

fn open_parent(app: &mut AppState, window_rows: usize) {
    let panel = app.get_active_panel();
    let Some(browser) = AppState::browser(panel) else {
        return;
    };
    let parent_index = browser.entries.iter().position(|e| e.name == "..");
    let Some(idx) = parent_index else { return };
    if browser.selected_index != idx {
        app.select_entry(idx, window_rows);
    }
    open_selected(app);
}

fn confirm_pending_op(app: &mut AppState) {
    if let Some(op) = app.take_pending_op() {
        if let PendingOp::Delete { target } = &op {
            if let Some(name) = target.file_name().and_then(|n| n.to_str()) {
                let parent = target.parent().unwrap_or_else(|| std::path::Path::new("."));
                app.fs_last_selected_name
                    .insert(parent.to_path_buf(), name.to_string());
            }
        }
        if let PendingOp::Rename { src } = &op {
            let name = app.rename_input.clone().unwrap_or_default();
            if name.is_empty()
                || name == "."
                || name == ".."
                || name.contains('/')
                || name.contains('\\')
            {
                app.clear_pending_op();
                return;
            }
            if let Some(current) = src.file_name().and_then(|n| n.to_str()) {
                if current == name {
                    app.clear_pending_op();
                    return;
                }
            }
            app.store_selection_memory_for(app.active_panel.clone());
            app.fs_last_selected_name.insert(
                src.parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf(),
                name,
            );
        }
        app.enqueue_pending_op(&op);
        match op {
            PendingOp::Copy { .. } | PendingOp::Move { .. } | PendingOp::Rename { .. } => {
                refresh_fs_panels(app)
            }
            PendingOp::Delete { .. } => refresh_active_panel(app),
        }
    }
}

fn refresh_active_panel(app: &mut AppState) {
    let which = app.active_panel.clone();
    let panel = app.panel(which.clone());
    let Some(browser) = AppState::browser(panel) else {
        return;
    };
    let path = browser.current_path.clone();
    if matches!(browser.browser_mode, BrowserMode::Fs) {
        load_fs_directory_async(app, path, which, None);
    }
}

fn refresh_fs_panels(app: &mut AppState) {
    for which in [ActivePanel::Left, ActivePanel::Right] {
        if let Some(browser) = AppState::browser(app.panel(which.clone())) {
            if !matches!(browser.browser_mode, BrowserMode::Fs) {
                continue;
            }
            let path = browser.current_path.clone();
            load_fs_directory_async(app, path, which, None);
        }
    }
}

fn draw_confirmation(ctx: &egui::Context, app: &mut AppState) {
    let op = match app.pending_op.clone() {
        Some(op) => op,
        None => return,
    };

    let colors = app.theme.colors();
    let screen = ctx.available_rect();

    let overlay_layer = egui::LayerId::new(egui::Order::Foreground, "confirm_overlay".into());
    ctx.layer_painter(overlay_layer).rect_filled(
        screen,
        egui::CornerRadius::ZERO,
        egui::Color32::from_black_alpha(160),
    );

    let (title, body) = pending_op_text(&op);
    let mut confirmed = false;
    let mut cancelled = false;
    let is_rename = matches!(op, PendingOp::Rename { .. });
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.colored_label(color32(colors.row_fg_active), body);
            if is_rename {
                ui.add_space(8.0);
                let mut name = app.rename_input.clone().unwrap_or_default();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut name)
                        .desired_width(260.0)
                        .hint_text("New name"),
                );
                if app.rename_focus {
                    response.request_focus();
                    app.rename_focus = false;
                }
                app.rename_input = Some(name);
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    confirmed = true;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let ok = ui.add(egui::Button::new("OK").min_size(egui::vec2(80.0, 0.0)));
                    let cancel =
                        ui.add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 0.0)));
                    if ok.clicked() {
                        confirmed = true;
                    }
                    if cancel.clicked() {
                        cancelled = true;
                    }
                });
            } else {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let yes = ui.add(egui::Button::new("Yes").min_size(egui::vec2(80.0, 0.0)));
                    let no = ui.add(egui::Button::new("No").min_size(egui::vec2(80.0, 0.0)));
                    if yes.clicked() {
                        confirmed = true;
                    }
                    if no.clicked() {
                        cancelled = true;
                    }
                });
            }
        });

    if confirmed {
        confirm_pending_op(app);
    } else if cancelled {
        app.clear_pending_op();
    }
}

fn draw_progress_modal(ctx: &egui::Context, app: &AppState) {
    if app.io_in_flight == 0 {
        return;
    }

    let colors = app.theme.colors();
    let screen = ctx.available_rect();

    let overlay_layer = egui::LayerId::new(egui::Order::Foreground, "progress_overlay".into());
    ctx.layer_painter(overlay_layer).rect_filled(
        screen,
        egui::CornerRadius::ZERO,
        egui::Color32::from_black_alpha(120),
    );

    egui::Window::new("Working")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            let label = if app.io_cancel_requested {
                "Cancelling…"
            } else {
                "Working…"
            };
            ui.colored_label(color32(colors.row_fg_active), label);
            ui.add_space(8.0);
            ui.add(egui::ProgressBar::new(0.0).animate(false));
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
            ui.add_space(6.0);
            ui.colored_label(color32(colors.row_fg_inactive), "Esc: cancel");
        });
}

fn draw_discard_modal(ctx: &egui::Context, app: &mut AppState) {
    let colors = app.theme.colors();
    let Some(side) = app.edit_panel_side() else {
        return;
    };
    let screen = ctx.available_rect();
    let overlay_layer = egui::LayerId::new(egui::Order::Foreground, "discard_overlay".into());
    ctx.layer_painter(overlay_layer).rect_filled(
        screen,
        egui::CornerRadius::ZERO,
        egui::Color32::from_black_alpha(120),
    );

    let mut action: Option<bool> = None;
    egui::Window::new("Discard changes?")
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::Vec2::new(360.0, 140.0))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(4.0);
                ui.colored_label(color32(colors.row_fg_active), "Discard unsaved edits?");
                ui.add_space(14.0);
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    let yes = egui::Button::new("Discard").min_size(egui::Vec2::new(120.0, 0.0));
                    let no =
                        egui::Button::new("Keep Editing").min_size(egui::Vec2::new(140.0, 0.0));
                    if ui.add(yes).clicked() {
                        action = Some(true);
                    }
                    if ui.add(no).clicked() {
                        action = Some(false);
                    }
                });
                ui.add_space(6.0);
            });
        });
    if let Some(accept) = action {
        let panel = app.panel_mut(side.clone());
        if accept {
            let return_focus = match &panel.mode {
                PanelMode::Edit(edit) => edit.return_focus.clone(),
                _ => side.clone(),
            };
            panel.mode = PanelMode::Browser;
            app.active_panel = return_focus;
        } else {
            if let PanelMode::Edit(edit) = &mut panel.mode {
                edit.confirm_discard = false;
            }
        }
    }
}

fn draw_editor(
    ui: &mut egui::Ui,
    theme: &Theme,
    is_focused: bool,
    edit: &mut EditState,
    highlight_cache: &HashMap<String, egui::text::LayoutJob>,
    highlight_pending: &mut HashSet<String>,
    highlight_req_tx: &mpsc::Sender<HighlightRequest>,
    min_height: f32,
) {
    let colors = theme.colors();
    egui::Frame::NONE
        .fill(color32(colors.preview_bg))
        .stroke(egui::Stroke::new(
            1.0,
            color32(if is_focused {
                colors.panel_border_active
            } else {
                colors.panel_border_inactive
            }),
        ))
        .show(ui, |ui| {
            ui.set_min_size(egui::Vec2::new(ui.available_width(), min_height));
            egui::Frame::NONE
                .fill(color32(colors.preview_header_bg))
                .show(ui, |ui| {
                    let title = edit
                        .path
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Edit".to_string());
                    ui.colored_label(color32(colors.preview_header_fg), format!("Edit — {title}"));
                });
            ui.add_space(4.0);
            if edit.loading {
                ui.colored_label(color32(colors.row_fg_inactive), "Loading…");
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(60));
                if is_focused {
                    ui.ctx().request_repaint();
                }
                return;
            }
            let mut text = std::mem::take(&mut edit.text);
            let edit_ext = edit.ext.clone();
            let theme_kind = theme.kind;
            let mut key = edit.highlight_key.clone();
            if key.is_none() {
                if let Some(path) = edit.path.as_ref() {
                    key = Some(format!("edit:{}", path.to_string_lossy()));
                    edit.highlight_key = key.clone();
                }
            }
            let key_with_hash = key
                .as_ref()
                .map(|base| format!("{base}:{}", edit.highlight_hash));
            if let Some(key) = key_with_hash.as_ref() {
                let cached = highlight_cache.contains_key(key);
                let ready = edit
                    .highlight_dirty_at
                    .map(|t| t.elapsed().as_millis() > 250)
                    .unwrap_or(true);
                if !cached && !highlight_pending.contains(key) && !text.is_empty() && ready {
                    edit.highlight_wrap_width = ui.available_width();
                    highlight_pending.insert(key.clone());
                    edit.highlight_dirty_at = None;
                    let _ = highlight_req_tx.send(HighlightRequest {
                        key: key.clone(),
                        text: text.clone(),
                        ext: edit_ext.clone(),
                        theme_kind,
                    });
                }
            }
            let mut needs_highlight = false;
            let mut layouter = |ui: &egui::Ui, string: &dyn egui::TextBuffer, wrap_width: f32| {
                if let Some(key) = key_with_hash.as_ref() {
                    let _wrap_changed = (edit.highlight_wrap_width - wrap_width).abs() > 0.5;
                    if let Some(cached) = highlight_cache.get(key) {
                        let mut job = cached.clone();
                        job.wrap.max_width = wrap_width;
                        return ui.fonts_mut(|f| f.layout_job(job));
                    }
                    needs_highlight = true;
                }
                ui.fonts_mut(|f| {
                    f.layout_job(egui::text::LayoutJob::simple(
                        string.as_str().to_string(),
                        egui::TextStyle::Monospace.resolve(ui.style()),
                        egui::Color32::LIGHT_GRAY,
                        wrap_width,
                    ))
                })
            };
            let footer_height = ui.text_style_height(&egui::TextStyle::Body).max(1.0) + 8.0;
            let editor_height = (ui.available_height() - footer_height).max(0.0);
            let row_height = ui.text_style_height(&egui::TextStyle::Monospace).max(1.0);
            let desired_rows = (editor_height / row_height).floor().max(1.0) as usize;
            let mut edit_response = None;
            egui::ScrollArea::vertical()
                .id_salt("editor_scroll")
                .auto_shrink([false, false])
                .max_height(editor_height)
                .show(ui, |ui| {
                    let output = egui::TextEdit::multiline(&mut text)
                        .font(egui::TextStyle::Monospace)
                        .layouter(&mut layouter)
                        .code_editor()
                        .id_source("editor_text")
                        .desired_width(f32::INFINITY)
                        .desired_rows(desired_rows)
                        .show(ui);
                    edit_response = Some(output.response);
                });
            let response = edit_response.unwrap_or_else(|| ui.label(" "));
            edit.text = text;
            if response.changed() {
                edit.highlight_hash = hash_text(&edit.text);
                edit.highlight_wrap_width = 0.0;
                edit.highlight_dirty_at = Some(std::time::Instant::now());
                edit.dirty = true;
            }
            if needs_highlight {
                if let Some(key) = edit.highlight_key.clone() {
                    let wrap_width = ui.available_width();
                    if !highlight_pending.contains(&key) {
                        edit.highlight_wrap_width = wrap_width;
                        highlight_pending.insert(key.clone());
                        let _ = highlight_req_tx.send(HighlightRequest {
                            key,
                            text: edit.text.clone(),
                            ext: edit_ext,
                            theme_kind,
                        });
                    }
                }
            }
            if is_focused {
                response.request_focus();
                ui.memory_mut(|mem| mem.request_focus(response.id));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(16));
            }
            ui.horizontal(|ui| {
                ui.colored_label(
                    color32(colors.row_fg_inactive),
                    "Ctrl+S: save  •  Esc: close",
                );
            });
        });
}
fn pending_op_text(op: &PendingOp) -> (&'static str, String) {
    match op {
        PendingOp::Copy { src, dst_dir, .. } => (
            "Confirm Copy",
            format!(
                "Copy \"{}\" to\n{}?",
                src.display_name(),
                dst_dir.to_string_lossy()
            ),
        ),
        PendingOp::Move { src, dst_dir } => (
            "Confirm Move",
            format!(
                "Move \"{}\" to\n{}?",
                src.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<unknown>"),
                dst_dir.to_string_lossy()
            ),
        ),
        PendingOp::Delete { target } => (
            "Confirm Delete",
            format!(
                "Delete \"{}\"?",
                target
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<unknown>")
            ),
        ),
        PendingOp::Rename { src } => (
            "Rename",
            format!(
                "Rename \"{}\" to:",
                src.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<unknown>")
            ),
        ),
    }
}

fn draw_preview(
    ui: &mut egui::Ui,
    theme: &Theme,
    is_focused: bool,
    preview: &mut PreviewState,
    image_cache: &mut ImageCache,
    image_req_tx: &mpsc::Sender<ImageRequest>,
    highlight_cache: &HashMap<String, egui::text::LayoutJob>,
    highlight_pending: &mut HashSet<String>,
    highlight_req_tx: &mpsc::Sender<HighlightRequest>,
    min_height: f32,
) {
    let colors = theme.colors();
    let header_bg = color32(colors.preview_header_bg);
    let header_fg = color32(colors.preview_header_fg);
    let text_color = color32(colors.preview_text);

    egui::Frame::NONE
        .fill(color32(colors.preview_bg))
        .stroke(egui::Stroke::new(
            1.0,
            color32(if is_focused {
                colors.panel_border_active
            } else {
                colors.panel_border_inactive
            }),
        ))
        .show(ui, |ui| {
            ui.set_min_size(egui::Vec2::new(ui.available_width(), min_height));
            egui::Frame::NONE.fill(header_bg).show(ui, |ui| {
                if is_focused {
                    ui.colored_label(header_fg, "● Preview (Tab to return)");
                } else {
                    ui.colored_label(header_fg, "Preview (Tab to focus)");
                }
            });
            ui.add_space(4.0);

            let page_height = ui.available_height();
            let output = egui::ScrollArea::both()
                .vertical_scroll_offset(preview.scroll)
                .show(ui, |ui| match preview.content.as_ref() {
                    Some(PreviewContent::Text(text)) => {
                        if preview.find_open {
                            ui.horizontal(|ui| {
                                ui.colored_label(text_color, "Find:");
                                let response = ui.text_edit_singleline(&mut preview.find_query);
                                if preview.find_focus {
                                    response.request_focus();
                                    preview.find_focus = false;
                                }
                            });
                            ui.add_space(4.0);
                        }
                        let ext = preview.ext.clone();
                        let base_key = preview.key.clone().unwrap_or_else(|| "unknown".to_string());
                        let key = format!("{base_key}:{:x}", hash_text(text));
                        if let Some(job) = highlight_cache.get(&key) {
                            ui.add(egui::Label::new(job.clone()).selectable(true));
                        } else {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new());
                                ui.colored_label(text_color, "Highlighting…");
                            });
                            ui.ctx()
                                .request_repaint_after(std::time::Duration::from_millis(120));
                            ui.add_space(6.0);
                            if highlight_pending.insert(key.clone()) {
                                let _ = highlight_req_tx.send(HighlightRequest {
                                    key: key.clone(),
                                    text: text.clone(),
                                    ext,
                                    theme_kind: theme.kind,
                                });
                            }
                            ui.colored_label(text_color, text);
                        }
                    }
                    Some(PreviewContent::Image(path)) => {
                        let (key, request) = match path {
                            ImageLocation::Fs(path) => {
                                let key = path.to_string_lossy().into_owned();
                                (
                                    key.clone(),
                                    ImageRequest {
                                        key,
                                        source: ImageSource::Fs(path.as_ref().to_path_buf()),
                                    },
                                )
                            }
                            ImageLocation::Container {
                                kind,
                                archive_path,
                                inner_path,
                            } => {
                                let key = format!(
                                    "{}::{}:/{}",
                                    archive_path.to_string_lossy(),
                                    match kind {
                                        ContainerKind::Zip => "zip",
                                        ContainerKind::TarGz => "tar.gz",
                                        ContainerKind::TarBz2 => "tar.bz2",
                                    },
                                    inner_path
                                );
                                (
                                    key.clone(),
                                    ImageRequest {
                                        key,
                                        source: ImageSource::Container {
                                            kind: *kind,
                                            archive_path: archive_path.clone(),
                                            inner_path: inner_path.clone(),
                                        },
                                    },
                                )
                            }
                        };
                        if let Some(handle) = image_cache.textures.get(&key).cloned() {
                            touch_image(image_cache, &key);
                            let sized = egui::load::SizedTexture::from_handle(&handle);
                            let available = ui.available_size();
                            let tex = sized.size;
                            let scale = (available.x / tex.x)
                                .min(available.y / tex.y)
                                .clamp(0.01, 1.0);
                            let size = egui::Vec2::new(tex.x * scale, tex.y * scale);
                            ui.add(egui::Image::new(sized).fit_to_exact_size(size));
                            ui.ctx().request_repaint();
                        } else {
                            if image_cache.pending.insert(key.clone()) {
                                let _ = image_req_tx.send(request);
                            }
                            ui.colored_label(text_color, format!("Loading image...\n{}", key));
                            ui.ctx()
                                .request_repaint_after(std::time::Duration::from_millis(120));
                        }
                    }
                    None => {
                        ui.colored_label(text_color, "No preview");
                    }
                });
            preview.scroll = output.state.offset.y;
            preview.page_height = page_height;
            preview.line_height = ui.text_style_height(&egui::TextStyle::Body);
            preview.can_scroll = output.content_size.y > output.inner_rect.height();
        });
}

fn draw_theme_picker(ctx: &egui::Context, app: &mut AppState) {
    let names = app.theme_names();
    let selected = app.theme_picker_selected.unwrap_or(0);

    egui::Window::new("Themes")
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            for (i, name) in names.iter().enumerate() {
                let is_selected = i == selected;
                let text = if is_selected {
                    egui::RichText::new(name).strong()
                } else {
                    egui::RichText::new(name)
                };
                if ui.selectable_label(is_selected, text).clicked() {
                    app.theme_picker_selected = Some(i);
                }
            }
        });
}

fn draw_command_bar(ctx: &egui::Context, colors: &ThemeColors) {
    let modifiers = ctx.input(|i| i.modifiers);
    egui::TopBottomPanel::bottom("command_bar")
        .exact_height(30.0)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(color32(colors.footer_bg))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (f3, f4, f5, f6, f7, f8) = if modifiers.alt {
                            ("", "", "Pack", "Unpack", "Search", "Command")
                        } else if modifiers.shift {
                            ("", "New", "Copy", "Rename", "", "")
                        } else {
                            ("View", "Edit", "Copy", "Move", "Mkdir", "Delete")
                        };
                        draw_key_cap(ui, "F3", f3, colors);
                        draw_key_cap(ui, "F4", f4, colors);
                        draw_key_cap(ui, "F5", f5, colors);
                        draw_key_cap(ui, "F6", f6, colors);
                        draw_key_cap(ui, "F7", f7, colors);
                        draw_key_cap(ui, "F8", f8, colors);
                    });
                });
        });
}

fn draw_key_cap(ui: &mut egui::Ui, key: &str, label: &str, colors: &ThemeColors) {
    let key_text = egui::RichText::new(key)
        .color(color32(colors.row_fg_selected))
        .strong();
    egui::Frame::NONE
        .fill(color32(colors.preview_header_bg))
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(key_text);
                if !label.is_empty() {
                    let label_text =
                        egui::RichText::new(format!(" {label}")).color(color32(colors.footer_fg));
                    ui.label(label_text);
                }
            });
        });
    ui.add_space(6.0);
}

fn draw_panel(
    ui: &mut egui::Ui,
    app: &mut AppState,
    panel_side: ActivePanel,
    _image_cache: &mut ImageCache,
    _image_req_tx: &mpsc::Sender<ImageRequest>,
    scroll_mode: ScrollMode,
    min_height: f32,
) -> usize {
    let available = ui.available_size();
    ui.set_min_size(available);
    let panel_height = available.y.max(0.0).max(min_height);
    let colors = app.theme.colors();
    let is_active = app.active_panel == panel_side;

    let (entries_len, selected_index, header_text, selected_label, loading, loading_progress) = {
        let panel = app.panel(panel_side.clone());
        let Some(browser) = AppState::browser(panel) else {
            return 0;
        };
        let entries_len = browser.entries.len();
        let selected_index = browser.selected_index;
        let header_text = format!(
            "{}    {}/{}",
            panel_path_display(panel),
            if entries_len == 0 {
                0
            } else {
                selected_index + 1
            },
            entries_len
        );
        let selected_label = browser
            .entries
            .get(selected_index)
            .map(|e| e.name.clone())
            .unwrap_or_else(|| "-".to_string());
        (
            entries_len,
            selected_index,
            header_text,
            selected_label,
            browser.loading,
            browser.loading_progress,
        )
    };

    let mut rows = 10usize;
    let mut clicked_index: Option<usize> = None;
    let mut open_on_double_click = false;
    let mut new_top_index: Option<usize> = None;
    let panel_side_for_closure = panel_side.clone();

    let panel_response = egui::Frame::NONE
        .fill(color32(Color::rgba(0.0, 0.0, 0.0, 0.0)))
        .stroke(egui::Stroke::new(
            1.0,
            color32(if is_active {
                colors.panel_border_active
            } else {
                colors.panel_border_inactive
            }),
        ))
        .show(ui, |ui| {
            ui.set_min_height(panel_height);
            ui.spacing_mut().item_spacing = egui::Vec2::new(6.0, 4.0);
            ui.vertical(|ui| {
                let header_height = 30.0;
                let footer_height = 24.0;
                let spacing = ui.spacing().item_spacing.y;

                ui.allocate_ui_with_layout(
                    egui::Vec2::new(ui.available_width(), header_height),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        egui::Frame::NONE
                            .fill(color32(colors.header_bg))
                            .corner_radius(egui::CornerRadius::same(4))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if is_active {
                                        ui.colored_label(color32(colors.header_fg), "●");
                                    }
                                    ui.colored_label(color32(colors.header_fg), header_text);
                                });
                            });
                    },
                );

                if loading {
                    let progress = loading_progress.unwrap_or((0, None));
                    let ratio = match progress.1 {
                        Some(total) if total > 0 => progress.0 as f32 / total as f32,
                        _ => 0.0,
                    };
                    ui.add_space(4.0);
                    let loading_label = AppState::browser(app.panel(panel_side.clone()))
                        .is_some_and(|browser| {
                            matches!(browser.browser_mode, BrowserMode::Search { .. })
                        });
                    let prefix = if loading_label {
                        "Searching…"
                    } else {
                        "Loading…"
                    };
                    let label = match progress.1 {
                        Some(total) => format!("{prefix} {}/{}", progress.0, total),
                        None => format!("{prefix} {}", progress.0),
                    };
                    let mut bar = egui::ProgressBar::new(ratio).text(label);
                    if progress.1.is_some() {
                        bar = bar.show_percentage();
                    }
                    ui.add(bar);
                }

                let list_height = (ui.available_height() - footer_height - spacing).max(0.0);
                rows = window_rows_for(list_height, ui.spacing().item_spacing.y);
                let mut visible_range = 0..0;

                let mut scroll_target = None;
                if is_active && scroll_mode == ScrollMode::ForceActive && entries_len > 0 {
                    let row_height = ROW_HEIGHT + ui.spacing().item_spacing.y;
                    let total_height =
                        (row_height * entries_len as f32 - ui.spacing().item_spacing.y).max(0.0);
                    let center_offset = (list_height - row_height) * 0.5;
                    let mut target = selected_index as f32 * row_height - center_offset;
                    if total_height > list_height {
                        let max_offset = (total_height - list_height).max(0.0);
                        if target < 0.0 {
                            target = 0.0;
                        } else if target > max_offset {
                            target = max_offset;
                        }
                    } else {
                        target = 0.0;
                    }
                    scroll_target = Some(target);
                }

                ui.allocate_ui_with_layout(
                    egui::Vec2::new(ui.available_width(), list_height),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        let mut scroll = egui::ScrollArea::vertical()
                            .id_salt(match panel_side_for_closure {
                                ActivePanel::Left => "left_list",
                                ActivePanel::Right => "right_list",
                            })
                            .auto_shrink([false, false]);
                        if let Some(offset) = scroll_target {
                            scroll = scroll.vertical_scroll_offset(offset);
                        }
                        scroll.show_rows(ui, ROW_HEIGHT, entries_len, |ui, row_range| {
                            visible_range = row_range.clone();
                            for idx in row_range {
                                let Some(browser) =
                                    AppState::browser(app.panel(panel_side_for_closure.clone()))
                                else {
                                    continue;
                                };
                                let entry = &browser.entries[idx];
                                let is_selected = selected_index == idx;
                                let stripe = idx % 2 == 0;
                                let bg = if is_selected {
                                    if is_active {
                                        colors.row_bg_selected_active
                                    } else {
                                        colors.row_bg_selected_inactive
                                    }
                                } else if stripe {
                                    Color::rgba(0.0, 0.0, 0.0, 0.06)
                                } else {
                                    Color::rgba(0.0, 0.0, 0.0, 0.0)
                                };
                                let fg = if is_selected {
                                    colors.row_fg_selected
                                } else if is_active {
                                    colors.row_fg_active
                                } else {
                                    colors.row_fg_inactive
                                };
                                let mut fg = fg;
                                if !is_selected && !entry.is_dir {
                                    let tint = if is_text_name(&entry.name) {
                                        Some(Color::rgba(0.25, 0.75, 0.55, 1.0))
                                    } else if is_media_name(&entry.name) {
                                        Some(Color::rgba(0.35, 0.65, 0.98, 1.0))
                                    } else {
                                        Some(Color::rgba(0.9, 0.7, 0.3, 1.0))
                                    };
                                    if let Some(tint) = tint {
                                        let factor = if is_active { 0.32 } else { 0.22 };
                                        fg = blend_color(fg, tint, factor);
                                    }
                                }

                                let (rect, response) = ui.allocate_exact_size(
                                    egui::Vec2::new(ui.available_width(), ROW_HEIGHT),
                                    egui::Sense::click(),
                                );
                                ui.painter().rect_filled(
                                    rect,
                                    egui::CornerRadius::same(3),
                                    color32(bg),
                                );

                                let icon_size = egui::Vec2::splat(10.0);
                                let icon_pos = rect.left_center()
                                    - egui::Vec2::new(0.0, icon_size.y * 0.5)
                                    + egui::Vec2::new(6.0, 0.0);
                                let icon_rect = egui::Rect::from_min_size(icon_pos, icon_size);
                                let icon_color = if entry.is_dir {
                                    colors.panel_border_active
                                } else {
                                    fg
                                };
                                ui.painter().rect_filled(
                                    icon_rect,
                                    egui::CornerRadius::same(2),
                                    color32(icon_color),
                                );

                                let font_id = egui::TextStyle::Body.resolve(ui.style());
                                let mut size_text = entry.size.map(format_size).unwrap_or_default();
                                if size_text.is_empty() && entry.is_dir {
                                    if let EntryLocation::Fs(path) = &entry.location {
                                        if app.dir_size_pending.contains(path) {
                                            size_text = "…".to_string();
                                        }
                                    }
                                }
                                if !size_text.is_empty() {
                                    ui.painter().text(
                                        egui::pos2(rect.right() - 8.0, rect.center().y),
                                        egui::Align2::RIGHT_CENTER,
                                        size_text,
                                        font_id.clone(),
                                        color32(fg),
                                    );
                                }
                                let name_min = rect.left_center() + egui::Vec2::new(22.0, 0.0);
                                let name_rect = egui::Rect::from_min_max(
                                    egui::pos2(name_min.x, rect.top()),
                                    egui::pos2(rect.right() - SIZE_COL_WIDTH, rect.bottom()),
                                );
                                ui.painter().with_clip_rect(name_rect).text(
                                    name_min,
                                    egui::Align2::LEFT_CENTER,
                                    entry.name.as_str(),
                                    font_id,
                                    color32(fg),
                                );

                                if response.clicked_by(egui::PointerButton::Primary) {
                                    clicked_index = Some(idx);
                                }
                                if response.double_clicked_by(egui::PointerButton::Primary) {
                                    clicked_index = Some(idx);
                                    open_on_double_click = true;
                                }
                            }
                        });
                    },
                );

                if entries_len > 0 {
                    new_top_index = Some(visible_range.start.min(entries_len - 1));
                } else {
                    new_top_index = Some(0);
                }

                ui.allocate_ui_with_layout(
                    egui::Vec2::new(ui.available_width(), footer_height),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        egui::Frame::NONE
                            .fill(color32(colors.footer_bg))
                            .corner_radius(egui::CornerRadius::same(4))
                            .show(ui, |ui| {
                                if is_active && app.search_ui == SearchUiState::Open {
                                    ui.horizontal(|ui| {
                                        ui.colored_label(color32(colors.footer_fg), "Search:");
                                        let response =
                                            ui.text_edit_singleline(&mut app.search_query);
                                        if app.search_focus {
                                            response.request_focus();
                                            app.search_focus = false;
                                        }
                                    });
                                } else {
                                    let footer_text = format!(
                                        "items: {entries_len} | selected: {selected_label}"
                                    );
                                    ui.colored_label(color32(colors.footer_fg), footer_text);
                                }
                            });
                    },
                );
            });
        });

    if panel_response.response.contains_pointer() && ui.input(|i| i.pointer.any_pressed()) {
        app.active_panel = panel_side.clone();
    }

    if let Some(top) = new_top_index {
        if let Some(browser) = AppState::browser_mut(app.panel_mut(panel_side.clone())) {
            browser.top_index = top;
        }
    }

    if let Some(idx) = clicked_index {
        app.active_panel = panel_side.clone();
        app.select_entry(idx, rows);
        if open_on_double_click {
            open_selected(app);
        }
    }

    rows
}

struct Runtime {
    window: Window,
    window_id: WindowId,
    context: Context,
    surface: blade_graphics::Surface,
    surface_config: SurfaceConfig,
    surface_info: blade_graphics::SurfaceInfo,
    command_encoder: blade_graphics::CommandEncoder,
    last_sync: Option<blade_graphics::SyncPoint>,
    egui_ctx: egui::Context,
    egui_state: EguiWinitState,
    painter: GuiPainter,
    size: winit::dpi::PhysicalSize<u32>,
    app: AppState,
    ui_cache: UiCache,
    image_cache: ImageCache,
    highlight_cache: HashMap<String, egui::text::LayoutJob>,
    highlight_pending: HashSet<String>,
    highlight_req_tx: mpsc::Sender<HighlightRequest>,
    highlight_res_rx: mpsc::Receiver<HighlightResult>,
    highlight_results: VecDeque<HighlightResult>,
    image_req_tx: mpsc::Sender<ImageRequest>,
    image_res_rx: mpsc::Receiver<ImageResult>,
    image_pending: VecDeque<ImageResult>,
    needs_redraw: bool,
}

impl Runtime {
    fn shutdown(&mut self) {
        self.image_cache.textures.clear();
        self.image_cache.order.clear();
        self.image_cache.pending.clear();
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
}

impl App {
    fn new() -> Self {
        Self { runtime: None }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.runtime.is_some() {
            return;
        }

        let window = event_loop
            .create_window(WindowAttributes::default().with_title("Fileman (egui)"))
            .expect("create window");
        let window_id = window.id();

        let context = unsafe {
            match Context::init(ContextDesc {
                presentation: true,
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
        let surface_config = SurfaceConfig {
            size: Extent {
                width: size.width.max(1),
                height: size.height.max(1),
                depth: 1,
            },
            usage: TextureUsage::TARGET,
            ..SurfaceConfig::default()
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
        let egui_state = EguiWinitState::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );

        let painter = GuiPainter::new(surface_info, &context);
        let command_encoder = context.create_command_encoder(CommandEncoderDesc {
            name: "egui",
            buffer_count: 1,
        });

        let cur_dir = std::env::current_dir().expect("current_dir");
        let (io_tx, io_rx, io_cancel_tx) = start_io_worker();
        let (preview_tx, preview_rx) = start_preview_worker();
        let (dir_size_tx, dir_size_rx) = start_dir_size_worker();
        let (search_tx, search_rx) = start_search_worker();
        let (image_req_tx, image_req_rx) = mpsc::channel::<ImageRequest>();
        let (image_res_tx, image_res_rx) = mpsc::channel::<ImageResult>();
        let (highlight_req_tx, highlight_req_rx) = mpsc::channel::<HighlightRequest>();
        let (highlight_res_tx, highlight_res_rx) = mpsc::channel::<HighlightResult>();
        let (edit_tx, edit_rx) = mpsc::channel::<EditLoadRequest>();
        let (edit_res_tx, edit_res_rx) = mpsc::channel::<EditLoadResult>();

        thread::spawn(move || {
            while let Ok(req) = image_req_rx.recv() {
                let image = match req.source {
                    ImageSource::Fs(path) => std::fs::read(path)
                        .ok()
                        .and_then(|data| decode_image_bytes(&data, MAX_TEXTURE_SIDE)),
                    ImageSource::Container {
                        kind,
                        archive_path,
                        inner_path,
                    } => {
                        let bytes = fileman::core::read_container_bytes_prefix(
                            kind,
                            &archive_path,
                            &inner_path,
                            usize::MAX,
                        )
                        .ok();
                        bytes.and_then(|data| decode_image_bytes(&data, MAX_TEXTURE_SIDE))
                    }
                };
                if let Some(image) = image {
                    let result = ImageResult {
                        key: req.key,
                        image,
                    };
                    let _ = image_res_tx.send(result);
                }
            }
        });

        thread::spawn(move || {
            while let Ok(req) = highlight_req_rx.recv() {
                let job = highlight_text_job(&req.text, req.ext.as_deref(), req.theme_kind);
                let _ = highlight_res_tx.send(HighlightResult { key: req.key, job });
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
                let _ = edit_res_tx.send(EditLoadResult {
                    id: req.id,
                    path: req.path,
                    text,
                });
            }
        });

        let mut app = AppState {
            left_panel: PanelState {
                browser: BrowserState {
                    browser_mode: BrowserMode::Fs,
                    current_path: cur_dir.clone(),
                    selected_index: 0,
                    entries: Vec::new(),
                    entries_rx: None,
                    prefer_select_name: None,
                    top_index: 0,
                    loading: false,
                    loading_progress: None,
                    dir_token: 0,
                    history_back: Vec::new(),
                    history_forward: Vec::new(),
                },
                mode: PanelMode::Browser,
            },
            right_panel: PanelState {
                browser: BrowserState {
                    browser_mode: BrowserMode::Fs,
                    current_path: cur_dir.clone(),
                    selected_index: 0,
                    entries: Vec::new(),
                    entries_rx: None,
                    prefer_select_name: None,
                    top_index: 0,
                    loading: false,
                    loading_progress: None,
                    dir_token: 0,
                    history_back: Vec::new(),
                    history_forward: Vec::new(),
                },
                mode: PanelMode::Browser,
            },
            active_panel: ActivePanel::Left,
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
            theme: Theme::dark(),
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
            search_case: SearchCase::Insensitive,
            search_mode: SearchMode::Name,
            search_results: Vec::new(),
            search_selected: 0,
            search_request_id: 0,
            search_status: SearchStatus::Idle,
            search_ui: SearchUiState::Closed,
            search_tx,
            search_rx,
        };

        app.theme
            .load_external_from_dir(std::path::Path::new("./themes"));
        load_fs_directory_async(&mut app, cur_dir.clone(), ActivePanel::Left, None);
        load_fs_directory_async(&mut app, cur_dir, ActivePanel::Right, None);

        let ui_cache = UiCache {
            left_rows: 10,
            right_rows: 10,
            scroll_mode: ScrollMode::Default,
            last_left_selected: 0,
            last_right_selected: 0,
            last_active_panel: ActivePanel::Left,
            last_left_dir_token: 0,
            last_right_dir_token: 0,
        };
        let image_cache = ImageCache {
            textures: HashMap::new(),
            pending: HashSet::new(),
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
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let runtime = match self.runtime.as_mut() {
            Some(runtime) if runtime.window_id == window_id => runtime,
            _ => return,
        };

        match event {
            WindowEvent::RedrawRequested => {
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
                }
                while let Ok(res) = runtime.highlight_res_rx.try_recv() {
                    runtime.highlight_cache.insert(res.key.clone(), res.job);
                    runtime.highlight_pending.remove(&res.key);
                }

                let raw_input = runtime.egui_state.take_egui_input(&runtime.window);
                let output = runtime.egui_ctx.run(raw_input, |ctx| {
                    apply_theme(ctx, &runtime.app.theme.colors());
                    let input = ctx.input(|i| i.clone());
                    handle_keyboard(ctx, &input, &mut runtime.app, &mut runtime.ui_cache);
                    runtime.ui_cache.update_scroll_mode(&runtime.app);

                    for decoded in decoded_images.drain(..) {
                        let handle = ctx.load_texture(
                            format!("preview:{}", decoded.key),
                            decoded.image,
                            egui::TextureOptions::LINEAR,
                        );
                        if !runtime.image_cache.textures.contains_key(&decoded.key) {
                            runtime.image_cache.order.push_back(decoded.key.clone());
                        }
                        runtime
                            .image_cache
                            .textures
                            .insert(decoded.key.clone(), handle);
                        runtime.image_cache.pending.remove(&decoded.key);
                        while runtime.image_cache.order.len() > MAX_IMAGE_TEXTURES {
                            if let Some(old) = runtime.image_cache.order.pop_front()
                                && old != decoded.key
                            {
                                runtime.image_cache.textures.remove(&old);
                            }
                        }
                        runtime.needs_redraw = true;
                    }

                    draw_command_bar(ctx, &runtime.app.theme.colors());

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
                            if should_show_editor(&runtime.app, ActivePanel::Left) {
                                let is_focused = runtime.app.active_panel == ActivePanel::Left;
                                let theme = runtime.app.theme.clone();
                                let panel = runtime.app.panel_mut(ActivePanel::Left);
                                if let PanelMode::Edit(edit) = &mut panel.mode {
                                    draw_editor(
                                        ui,
                                        &theme,
                                        is_focused,
                                        edit,
                                        &runtime.highlight_cache,
                                        &mut runtime.highlight_pending,
                                        &runtime.highlight_req_tx,
                                        rect.height(),
                                    );
                                }
                            } else if should_show_preview(&runtime.app, ActivePanel::Left) {
                                let is_focused = runtime.app.active_panel == ActivePanel::Left;
                                let theme = runtime.app.theme.clone();
                                let panel = runtime.app.panel_mut(ActivePanel::Left);
                                if let PanelMode::Preview(preview) = &mut panel.mode {
                                    draw_preview(
                                        ui,
                                        &theme,
                                        is_focused,
                                        preview,
                                        &mut runtime.image_cache,
                                        &runtime.image_req_tx,
                                        &runtime.highlight_cache,
                                        &mut runtime.highlight_pending,
                                        &runtime.highlight_req_tx,
                                        rect.height(),
                                    );
                                }
                            } else {
                                runtime.ui_cache.left_rows = draw_panel(
                                    ui,
                                    &mut runtime.app,
                                    ActivePanel::Left,
                                    &mut runtime.image_cache,
                                    &runtime.image_req_tx,
                                    runtime.ui_cache.scroll_mode,
                                    rect.height(),
                                );
                            }
                        });
                        ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
                            if should_show_editor(&runtime.app, ActivePanel::Right) {
                                let is_focused = runtime.app.active_panel == ActivePanel::Right;
                                let theme = runtime.app.theme.clone();
                                let panel = runtime.app.panel_mut(ActivePanel::Right);
                                if let PanelMode::Edit(edit) = &mut panel.mode {
                                    draw_editor(
                                        ui,
                                        &theme,
                                        is_focused,
                                        edit,
                                        &runtime.highlight_cache,
                                        &mut runtime.highlight_pending,
                                        &runtime.highlight_req_tx,
                                        rect.height(),
                                    );
                                }
                            } else if should_show_preview(&runtime.app, ActivePanel::Right) {
                                let is_focused = runtime.app.active_panel == ActivePanel::Right;
                                let theme = runtime.app.theme.clone();
                                let panel = runtime.app.panel_mut(ActivePanel::Right);
                                if let PanelMode::Preview(preview) = &mut panel.mode {
                                    draw_preview(
                                        ui,
                                        &theme,
                                        is_focused,
                                        preview,
                                        &mut runtime.image_cache,
                                        &runtime.image_req_tx,
                                        &runtime.highlight_cache,
                                        &mut runtime.highlight_pending,
                                        &runtime.highlight_req_tx,
                                        rect.height(),
                                    );
                                }
                            } else {
                                runtime.ui_cache.right_rows = draw_panel(
                                    ui,
                                    &mut runtime.app,
                                    ActivePanel::Right,
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
                        draw_theme_picker(ctx, &mut runtime.app);
                    }
                    if runtime.app.pending_op.is_some() {
                        draw_confirmation(ctx, &mut runtime.app);
                    }
                    if let Some(edit) = runtime.app.edit_panel_mut() {
                        if edit.confirm_discard {
                            draw_discard_modal(ctx, &mut runtime.app);
                        }
                    }
                    if runtime.app.io_in_flight > 0 {
                        draw_progress_modal(ctx, &runtime.app);
                    }
                });
                runtime
                    .egui_state
                    .handle_platform_output(&runtime.window, output.platform_output);

                let paint_jobs = runtime
                    .egui_ctx
                    .tessellate(output.shapes, output.pixels_per_point);
                let screen_descriptor = ScreenDescriptor {
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
                    TextureViewDesc {
                        name: "surface",
                        format: runtime.surface_info.format,
                        dimension: ViewDimension::D2,
                        subresources: &TextureSubresources::default(),
                    },
                );

                let mut render = runtime.command_encoder.render(
                    "egui",
                    RenderTargetSet {
                        colors: &[RenderTarget {
                            view,
                            init_op: InitOp::Clear(TextureColor::TransparentBlack),
                            finish_op: FinishOp::Store,
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
            }
            other => {
                let event_response = runtime.egui_state.on_window_event(&runtime.window, &other);
                if event_response.repaint {
                    runtime.needs_redraw = true;
                }
                if event_response.consumed {
                    runtime.window.request_redraw();
                    return;
                }

                match other {
                    WindowEvent::CloseRequested => event_loop.exit(),
                    WindowEvent::Resized(new_size) => {
                        runtime.size = new_size;
                        runtime.surface_config.size = Extent {
                            width: runtime.size.width.max(1),
                            height: runtime.size.height.max(1),
                            depth: 1,
                        };
                        runtime
                            .context
                            .reconfigure_surface(&mut runtime.surface, runtime.surface_config);
                        runtime.needs_redraw = true;
                    }
                    _ => {
                        runtime.needs_redraw = true;
                    }
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Wait);
        if let Some(runtime) = self.runtime.as_mut() {
            while let Ok(img) = runtime.image_res_rx.try_recv() {
                runtime.image_pending.push_back(img);
                runtime.needs_redraw = true;
            }
            while let Ok(res) = runtime.highlight_res_rx.try_recv() {
                runtime.highlight_results.push_back(res);
                runtime.needs_redraw = true;
            }
            if let Some(preview) = runtime.app.preview_panel_mut() {
                if let Some(PreviewContent::Image(path)) = preview.content.as_ref() {
                    let key = match path {
                        ImageLocation::Fs(path) => path.to_string_lossy().into_owned(),
                        ImageLocation::Container {
                            kind,
                            archive_path,
                            inner_path,
                        } => format!(
                            "{}::{}:/{}",
                            archive_path.to_string_lossy(),
                            match kind {
                                ContainerKind::Zip => "zip",
                                ContainerKind::TarGz => "tar.gz",
                                ContainerKind::TarBz2 => "tar.bz2",
                            },
                            inner_path
                        ),
                    };
                    if runtime.image_cache.pending.contains(&key)
                        || runtime.image_cache.textures.get(&key).is_none()
                    {
                        runtime.needs_redraw = true;
                    }
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

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.shutdown();
        }
    }
}

fn parse_snapshot_arg() -> Result<Option<PathBuf>> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--snapshot" {
            return args
                .next()
                .map(|value| Ok(Some(PathBuf::from(value))))
                .unwrap_or_else(|| Err(anyhow::anyhow!("--snapshot requires a path")));
        }
    }
    Ok(None)
}

fn run_snapshot(path: &PathBuf) -> Result<()> {
    let context = unsafe {
        Context::init(ContextDesc::default())
            .map_err(|err| anyhow::anyhow!("Failed to init GPU context: {err:?}"))?
    };

    let size = Extent {
        width: SNAPSHOT_WIDTH,
        height: SNAPSHOT_HEIGHT,
        depth: 1,
    };
    let format = TextureFormat::Rgba8Unorm;
    let surface_info = SurfaceInfo {
        format,
        alpha: AlphaMode::PreMultiplied,
    };
    let mut painter = GuiPainter::new(surface_info, &context);
    let mut command_encoder = context.create_command_encoder(CommandEncoderDesc {
        name: "snapshot",
        buffer_count: 1,
    });

    let texture = context.create_texture(TextureDesc {
        name: "snapshot_target",
        format,
        size,
        array_layer_count: 1,
        mip_level_count: 1,
        sample_count: 1,
        dimension: blade_graphics::TextureDimension::D2,
        usage: TextureUsage::TARGET | TextureUsage::COPY,
        external: None,
    });
    let view = context.create_texture_view(
        texture,
        TextureViewDesc {
            name: "snapshot_view",
            format,
            dimension: ViewDimension::D2,
            subresources: &TextureSubresources::default(),
        },
    );

    let cur_dir = std::env::current_dir()?;
    let entries = read_fs_directory(cur_dir.as_path()).unwrap_or_default();

    let (preview_tx, _preview_req_rx) = mpsc::channel::<PreviewRequest>();
    let (_preview_res_tx, preview_rx) = mpsc::channel::<(u64, PreviewContent)>();
    let (io_tx, _io_rx_unused) = mpsc::channel::<fileman::core::IOTask>();
    let (_io_res_tx, io_rx) = mpsc::channel::<fileman::core::IOResult>();
    let (io_cancel_tx, _io_cancel_rx) = mpsc::channel::<()>();
    let (dir_size_tx, _dir_size_req_rx) = mpsc::channel::<PathBuf>();
    let (_dir_size_res_tx, dir_size_rx) = mpsc::channel::<(PathBuf, u64)>();
    let (edit_tx, _edit_req_rx) = mpsc::channel::<EditLoadRequest>();
    let (_edit_res_tx, edit_res_rx) = mpsc::channel::<EditLoadResult>();
    let (search_tx, _search_req_rx) = mpsc::channel::<SearchRequest>();
    let (_search_res_tx, search_rx) = mpsc::channel::<SearchEvent>();
    let (image_req_tx, _image_req_rx) = mpsc::channel::<ImageRequest>();
    let (highlight_req_tx, _highlight_req_rx) = mpsc::channel::<HighlightRequest>();
    let mut image_cache = ImageCache {
        textures: HashMap::new(),
        pending: HashSet::new(),
        order: VecDeque::new(),
    };
    let highlight_cache = HashMap::new();
    let mut highlight_pending = HashSet::new();

    let mut app = AppState {
        left_panel: PanelState {
            browser: BrowserState {
                browser_mode: BrowserMode::Fs,
                current_path: cur_dir.clone(),
                selected_index: 0,
                entries: entries.clone(),
                entries_rx: None,
                prefer_select_name: None,
                top_index: 0,
                loading: false,
                loading_progress: None,
                dir_token: 0,
                history_back: Vec::new(),
                history_forward: Vec::new(),
            },
            mode: PanelMode::Browser,
        },
        right_panel: PanelState {
            browser: BrowserState {
                browser_mode: BrowserMode::Fs,
                current_path: cur_dir.clone(),
                selected_index: 0,
                entries,
                entries_rx: None,
                prefer_select_name: None,
                top_index: 0,
                loading: false,
                loading_progress: None,
                dir_token: 0,
                history_back: Vec::new(),
                history_forward: Vec::new(),
            },
            mode: PanelMode::Browser,
        },
        active_panel: ActivePanel::Left,
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
        theme: Theme::dark(),
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
        search_case: SearchCase::Insensitive,
        search_mode: SearchMode::Name,
        search_results: Vec::new(),
        search_selected: 0,
        search_request_id: 0,
        search_status: SearchStatus::Idle,
        search_ui: SearchUiState::Closed,
        search_tx,
        search_rx,
    };
    app.theme
        .load_external_from_dir(std::path::Path::new("./themes"));

    let egui_ctx = egui::Context::default();
    let raw_input = egui::RawInput {
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
    let output = egui_ctx.run(raw_input, |ctx| {
        apply_theme(ctx, &app.theme.colors());
        draw_command_bar(ctx, &app.theme.colors());
        let _ui_cache = UiCache {
            left_rows: 10,
            right_rows: 10,
            scroll_mode: ScrollMode::Default,
            last_left_selected: 0,
            last_right_selected: 0,
            last_active_panel: ActivePanel::Left,
            last_left_dir_token: 0,
            last_right_dir_token: 0,
        };
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

            ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
                if should_show_editor(&app, ActivePanel::Left) {
                    let is_focused = app.active_panel == ActivePanel::Left;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(ActivePanel::Left);
                    if let PanelMode::Edit(edit) = &mut panel.mode {
                        draw_editor(
                            ui,
                            &theme,
                            is_focused,
                            edit,
                            &highlight_cache,
                            &mut highlight_pending,
                            &highlight_req_tx,
                            rect.height(),
                        );
                    }
                } else if should_show_preview(&app, ActivePanel::Left) {
                    let is_focused = app.active_panel == ActivePanel::Left;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(ActivePanel::Left);
                    if let PanelMode::Preview(preview) = &mut panel.mode {
                        draw_preview(
                            ui,
                            &theme,
                            is_focused,
                            preview,
                            &mut image_cache,
                            &image_req_tx,
                            &highlight_cache,
                            &mut highlight_pending,
                            &highlight_req_tx,
                            rect.height(),
                        );
                    }
                } else {
                    draw_panel(
                        ui,
                        &mut app,
                        ActivePanel::Left,
                        &mut image_cache,
                        &image_req_tx,
                        ScrollMode::Default,
                        rect.height(),
                    );
                }
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
                if should_show_editor(&app, ActivePanel::Right) {
                    let is_focused = app.active_panel == ActivePanel::Right;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(ActivePanel::Right);
                    if let PanelMode::Edit(edit) = &mut panel.mode {
                        draw_editor(
                            ui,
                            &theme,
                            is_focused,
                            edit,
                            &highlight_cache,
                            &mut highlight_pending,
                            &highlight_req_tx,
                            rect.height(),
                        );
                    }
                } else if should_show_preview(&app, ActivePanel::Right) {
                    let is_focused = app.active_panel == ActivePanel::Right;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(ActivePanel::Right);
                    if let PanelMode::Preview(preview) = &mut panel.mode {
                        draw_preview(
                            ui,
                            &theme,
                            is_focused,
                            preview,
                            &mut image_cache,
                            &image_req_tx,
                            &highlight_cache,
                            &mut highlight_pending,
                            &highlight_req_tx,
                            rect.height(),
                        );
                    }
                } else {
                    draw_panel(
                        ui,
                        &mut app,
                        ActivePanel::Right,
                        &mut image_cache,
                        &image_req_tx,
                        ScrollMode::Default,
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
                color32(app.theme.colors().divider),
            );
        });
        if app.pending_op.is_some() {
            draw_confirmation(ctx, &mut app);
        }
        if let Some(edit) = app.edit_panel_mut() {
            if edit.confirm_discard {
                draw_discard_modal(ctx, &mut app);
            }
        }
        if app.io_in_flight > 0 {
            draw_progress_modal(ctx, &app);
        }
    });

    let paint_jobs = egui_ctx.tessellate(output.shapes, output.pixels_per_point);
    let screen_descriptor = ScreenDescriptor {
        physical_size: (SNAPSHOT_WIDTH, SNAPSHOT_HEIGHT),
        scale_factor: 1.0,
    };

    command_encoder.start();
    command_encoder.init_texture(texture);
    painter.update_textures(&mut command_encoder, &output.textures_delta, &context);
    let mut render = command_encoder.render(
        "snapshot",
        RenderTargetSet {
            colors: &[RenderTarget {
                view,
                init_op: InitOp::Clear(TextureColor::TransparentBlack),
                finish_op: FinishOp::Store,
            }],
            depth_stencil: None,
        },
    );
    painter.paint(&mut render, &paint_jobs, &screen_descriptor, &context);
    drop(render);

    let bytes_per_row = align_to(SNAPSHOT_WIDTH * 4, 256);
    let buffer_size = bytes_per_row as u64 * SNAPSHOT_HEIGHT as u64;
    let result_buffer = context.create_buffer(blade_graphics::BufferDesc {
        name: "snapshot_readback",
        size: buffer_size,
        memory: blade_graphics::Memory::Shared,
    });

    {
        let mut transfer = command_encoder.transfer("snapshot readback");
        transfer.copy_texture_to_buffer(
            blade_graphics::TexturePiece {
                texture,
                mip_level: 0,
                array_layer: 0,
                origin: [0, 0, 0],
            },
            result_buffer.into(),
            bytes_per_row,
            size,
        );
    }

    let sync = context.submit(&mut command_encoder);
    painter.after_submit(&sync);
    context.wait_for(&sync, !0);

    save_snapshot_png(
        &result_buffer,
        SNAPSHOT_WIDTH,
        SNAPSHOT_HEIGHT,
        bytes_per_row as usize,
        path,
    )?;

    context.destroy_texture_view(view);
    context.destroy_texture(texture);
    context.destroy_buffer(result_buffer);
    painter.destroy(&context);
    context.destroy_command_encoder(&mut command_encoder);

    Ok(())
}

fn align_to(value: u32, alignment: u32) -> u32 {
    ((value + alignment - 1) / alignment) * alignment
}

fn save_snapshot_png(
    buffer: &blade_graphics::Buffer,
    width: u32,
    height: u32,
    bytes_per_row: usize,
    path: &PathBuf,
) -> Result<()> {
    let row_bytes = (width * 4) as usize;
    let mut data = vec![0u8; row_bytes * height as usize];
    let src = buffer.data() as *const u8;
    for y in 0..height as usize {
        let src_row = unsafe { std::slice::from_raw_parts(src.add(y * bytes_per_row), row_bytes) };
        let dst_row = &mut data[y * row_bytes..(y + 1) * row_bytes];
        dst_row.copy_from_slice(src_row);
    }

    let image = ZuneImage::from_u8(&data, width as usize, height as usize, ColorSpace::RGBA);
    image
        .save_to(path, ImageFormat::PNG)
        .map_err(|err| anyhow::anyhow!(format!("Failed to encode snapshot: {err:?}")))?;
    Ok(())
}

fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_module("egui", log::LevelFilter::Warn)
        .filter_module("egui_winit", log::LevelFilter::Warn)
        .init();

    if let Some(snapshot_path) = parse_snapshot_arg()? {
        return run_snapshot(&snapshot_path);
    }

    let event_loop = EventLoop::new()?;
    let mut app = App::new();
    event_loop
        .run_app(&mut app)
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}
