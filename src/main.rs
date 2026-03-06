use anyhow::Result;
use blade_egui::{GuiPainter, ScreenDescriptor};
use blade_graphics::{
    AlphaMode, CommandEncoderDesc, Context, ContextDesc, Extent, FinishOp, InitOp, RenderTarget,
    RenderTargetSet, SurfaceConfig, SurfaceInfo, TextureColor, TextureDesc, TextureFormat,
    TextureSubresources, TextureUsage, TextureViewDesc, ViewDimension,
};
use egui::Color32;
use egui::text_edit::TextEditOutput;
use egui_winit::State as EguiWinitState;
use exif::{Tag, Value};
use gif::{ColorOutput, DecodeOptions};
use image_webp::WebPDecoder;
use once_cell::sync::Lazy;
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    fs,
    hash::{Hash, Hasher},
    io::Read,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::UNIX_EPOCH,
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
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Icon, Window, WindowAttributes, WindowId},
};
use zune_bmp::BmpDecoder;
use zune_core::bit_depth::BitDepth;
use zune_core::{colorspace::ColorSpace, options::DecoderOptions};
use zune_image::image::Image as ZuneImage;

use fileman::app_state::{
    AppState, BrowserState, EditState, FileProps, FilePropsEdit, PanelMode, PanelState, PendingOp,
    PreviewState, PropsDialog, SearchStatus, SearchUiState,
};
use fileman::core::{
    ActivePanel, BrowserMode, ContainerKind, DirBatch, DirEntry, EditLoadRequest, EditLoadResult,
    EntryLocation, ImageLocation, PreviewContent, PreviewRequest, SearchCase, SearchEvent,
    SearchMode, SearchProgress, SearchRequest, SearchResult, SortMode, container_display_path,
    container_kind_from_path, format_size, hexdump_with_width, is_media_name, is_text_name,
    read_container_directory_with_progress,
};
use fileman::theme::{Color, Theme, ThemeColors, ThemeKind};
use fileman::workers::{
    start_dir_size_worker, start_io_worker, start_preview_worker, start_search_worker,
};
use replay::{FileAssert, FsAssert, FsCheckMode, FsEntryKind, ReplayAsserts, SnapshotAssert};
mod replay;
use fileman::snapshot::{align_to, compare_snapshots, save_snapshot_png};
use replay::{ReplayKey, load_replay_case};
use users::{get_group_by_gid, get_group_by_name, get_user_by_name, get_user_by_uid};

const ROW_HEIGHT: f32 = 24.0;
const SIZE_COL_WIDTH: f32 = 84.0;
const SNAPSHOT_WIDTH: u32 = 800;
const SNAPSHOT_HEIGHT: u32 = 600;
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContainerLoadMode {
    UseCache,
    ForceReload,
}

impl UiCache {
    fn update_scroll_mode(&mut self, app: &AppState) {
        let left_selected = app.left_panel.browser.selected_index;
        let right_selected = app.right_panel.browser.selected_index;
        let active = app.active_panel;
        let left_dir = app.left_panel.browser.dir_token;
        let right_dir = app.right_panel.browser.dir_token;
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

struct ImageMeta {
    width: usize,
    height: usize,
    depth: BitDepth,
}

struct ImageResult {
    key: String,
    image: egui::ColorImage,
    meta: ImageMeta,
}

enum ImageResponse {
    Ok(ImageResult),
    Err { key: String, message: String },
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

#[derive(Default)]
struct ImageCache {
    textures: HashMap<String, egui::TextureHandle>,
    meta: HashMap<String, ImageMeta>,
    failures: HashMap<String, String>,
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

fn decode_image_bytes(bytes: &[u8], max_side: u32) -> Option<(egui::ColorImage, ImageMeta)> {
    let options = DecoderOptions::new_fast();
    if let Ok(image) = ZuneImage::read(bytes, options) {
        let orientation = exif_orientation(&image).unwrap_or(1);
        let (width, height) = image.dimensions();
        let depth = image.depth();
        let colorspace = image.colorspace();
        let mut frames = image.flatten_to_u8();
        let data = frames.pop()?;
        let rgba = convert_to_rgba(&data, width, height, colorspace)?;
        let (rgba, width, height) = apply_orientation_rgba(rgba, width, height, orientation);
        let (out_w, out_h, out_rgba) = downscale_rgba(&rgba, width, height, max_side);
        let color = egui::ColorImage::from_rgba_unmultiplied([out_w, out_h], &out_rgba);
        return Some((
            color,
            ImageMeta {
                width,
                height,
                depth,
            },
        ));
    }

    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return decode_webp_bytes(bytes, max_side);
    }

    if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return decode_gif_bytes(bytes, max_side);
    }

    if bytes.len() >= 2 && bytes[0] == b'B' && bytes[1] == b'M' {
        return decode_bmp_bytes(bytes, max_side);
    }

    None
}

fn decode_gif_bytes(bytes: &[u8], max_side: u32) -> Option<(egui::ColorImage, ImageMeta)> {
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    let cursor = std::io::Cursor::new(bytes);
    let mut decoder = options.read_info(cursor).ok()?;
    let frame = decoder.read_next_frame().ok()??;
    let width = usize::from(frame.width);
    let height = usize::from(frame.height);
    let rgba = frame.buffer.to_vec();
    if rgba.len() != width * height * 4 {
        return None;
    }
    let (out_w, out_h, out_rgba) = downscale_rgba(&rgba, width, height, max_side);
    let color = egui::ColorImage::from_rgba_unmultiplied([out_w, out_h], &out_rgba);
    Some((
        color,
        ImageMeta {
            width,
            height,
            depth: BitDepth::Eight,
        },
    ))
}

fn decode_webp_bytes(bytes: &[u8], max_side: u32) -> Option<(egui::ColorImage, ImageMeta)> {
    let cursor = std::io::Cursor::new(bytes);
    let mut decoder = WebPDecoder::new(cursor).ok()?;
    let size = decoder.output_buffer_size()?;
    let mut data = vec![0u8; size];
    decoder.read_image(&mut data).ok()?;
    let (width, height) = decoder.dimensions();
    let width = width as usize;
    let height = height as usize;
    let has_alpha = decoder.has_alpha();
    let rgba = if has_alpha {
        data
    } else {
        let mut out = Vec::with_capacity(width * height * 4);
        for rgb in data.chunks_exact(3) {
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        out
    };
    let (out_w, out_h, out_rgba) = downscale_rgba(&rgba, width, height, max_side);
    let color = egui::ColorImage::from_rgba_unmultiplied([out_w, out_h], &out_rgba);
    Some((
        color,
        ImageMeta {
            width,
            height,
            depth: BitDepth::Eight,
        },
    ))
}

fn decode_bmp_bytes(bytes: &[u8], max_side: u32) -> Option<(egui::ColorImage, ImageMeta)> {
    let mut decoder = BmpDecoder::new(bytes);
    decoder.decode_headers().ok()?;
    let (width, height) = decoder.get_dimensions()?;
    let depth = decoder.get_depth();
    let colorspace = decoder.get_colorspace()?;
    let data = decoder.decode().ok()?;
    let rgba = convert_to_rgba(&data, width, height, colorspace)?;
    let (out_w, out_h, out_rgba) = downscale_rgba(&rgba, width, height, max_side);
    let color = egui::ColorImage::from_rgba_unmultiplied([out_w, out_h], &out_rgba);
    Some((
        color,
        ImageMeta {
            width,
            height,
            depth,
        },
    ))
}

fn exif_orientation(image: &ZuneImage) -> Option<u16> {
    let exif = image.metadata().exif()?;
    for field in exif {
        if field.tag == Tag::Orientation
            && let Value::Short(values) = &field.value
        {
            return values.first().copied();
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

fn app_icon() -> Option<Icon> {
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
    Icon::from_rgba(rgba, size, size).ok()
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
    let browser = &panel.browser;
    let BrowserState {
        browser_mode: ref mode,
        ..
    } = *browser;
    match mode {
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

fn sort_entries(entries: &mut Vec<DirEntry>, mode: SortMode, descending: bool) {
    if mode == SortMode::Raw {
        return;
    }

    let parent_index = entries.iter().position(|entry| entry.name == "..");
    let parent = parent_index.map(|idx| entries.remove(idx));

    entries.sort_by(|a, b| {
        if a.is_dir != b.is_dir {
            return b.is_dir.cmp(&a.is_dir);
        }
        let mut ord = match mode {
            SortMode::Name => {
                if descending {
                    b.name.cmp(&a.name)
                } else {
                    a.name.cmp(&b.name)
                }
            }
            SortMode::Date => cmp_option_u64(a.modified, b.modified, descending),
            SortMode::Size => {
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
            SortMode::Raw => Ordering::Equal,
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

fn resort_browser_entries(browser: &mut BrowserState) {
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

fn sort_mode_label(mode: SortMode) -> &'static str {
    match mode {
        SortMode::Name => "Name",
        SortMode::Date => "Date",
        SortMode::Size => "Size",
        SortMode::Raw => "Raw",
    }
}

fn rebuild_search_entries(browser: &mut BrowserState, results: &[SearchResult]) {
    let BrowserState {
        browser_mode: ref mode,
        ..
    } = *browser;
    browser.entries = results
        .iter()
        .map(|result| {
            let display_name = match mode {
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
                modified: result.modified,
            }
        })
        .collect();
}

fn hexdump_job(
    bytes: &[u8],
    width: usize,
    colors: &ThemeColors,
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
                modified: None,
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
    sort_entries(&mut browser.entries, browser.sort_mode, browser.sort_desc);
    if let Some(pref) = restore_name
        && let Some(idx) = browser.entries.iter().position(|e| e.name == pref)
    {
        browser.selected_index = idx;
    }
    if browser.selected_index < browser.top_index {
        browser.top_index = browser.selected_index;
    }
}

fn pump_async(app: &mut AppState) -> bool {
    let mut changed = false;
    for side in [ActivePanel::Left, ActivePanel::Right] {
        let panel = app.panel_mut(side);
        let browser = &mut panel.browser;
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

    if let Ok((id, content)) = app.preview_rx.try_recv()
        && let Some(preview) = app.preview_panel_mut()
        && id == preview.request_id
    {
        preview.content = Some(content);
        changed = true;
    }

    while let Ok((path, size)) = app.dir_size_rx.try_recv() {
        app.dir_size_pending.remove(&path);
        app.dir_sizes.insert(path.clone(), size);
        for side in [ActivePanel::Left, ActivePanel::Right] {
            let panel = app.panel_mut(side);
            let browser = &mut panel.browser;
            let mut updated = false;
            for entry in &mut browser.entries {
                if entry.is_dir
                    && let EntryLocation::Fs(p) = &entry.location
                    && *p == path
                {
                    entry.size = Some(size);
                    updated = true;
                }
            }
            if updated && browser.sort_mode == SortMode::Size {
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
                    let browser = &mut panel.browser;
                    let BrowserState {
                        browser_mode: ref mode,
                        ..
                    } = *browser;
                    let display_name = match mode {
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
                        modified: result.modified,
                    });
                    resort_browser_entries(browser);
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
                    panel.browser.loading_progress =
                        Some((progress.matched, Some(progress.scanned)));
                    changed = true;
                }
            }
            SearchEvent::Done { id, progress } => {
                if id == app.search_request_id {
                    app.search_status = SearchStatus::Done(progress);
                    let panel = app.get_active_panel_mut();
                    panel.browser.loading = false;
                    panel.browser.loading_progress =
                        Some((progress.matched, Some(progress.scanned)));
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
                    panel.browser.loading = false;
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
            modified: None,
        });
        has_parent_entry = true;
    }

    app.stash_container_cache(target_panel);
    let (tx, rx) = mpsc::channel::<DirBatch>();
    let path_clone = path.clone();
    let wake = app.wake.clone();
    let dir_sizes_snapshot = app.dir_sizes.clone();
    let dir_sizes_fallback = app.dir_sizes.clone();

    if let Ok(mut rd) = fs::read_dir(&path) {
        let mut snapshot: Vec<DirEntry> = Vec::with_capacity(128);
        for _ in 0..128 {
            match rd.next() {
                Some(Ok(entry)) => {
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                    let metadata = entry.metadata().ok();
                    let size = if is_dir {
                        dir_sizes_snapshot.get(&entry.path()).copied()
                    } else {
                        metadata.as_ref().map(|m| m.len())
                    };
                    let modified = metadata
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    snapshot.push(DirEntry {
                        name: file_name,
                        is_dir,
                        location: EntryLocation::Fs(entry.path()),
                        size,
                        modified,
                    });
                }
                Some(Err(_)) | None => break,
            }
        }
        if !snapshot.is_empty() {
            let _ = tx.send(DirBatch::Append(snapshot.clone()));
        }
        thread::spawn(move || {
            let chunk = 500usize;
            let mut all: Vec<DirEntry> = snapshot;
            for entry in rd.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();
                if let Ok(file_type) = entry.file_type() {
                    let is_dir = file_type.is_dir();
                    let metadata = entry.metadata().ok();
                    let size = if is_dir {
                        dir_sizes_snapshot.get(&entry.path()).copied()
                    } else {
                        metadata.as_ref().map(|m| m.len())
                    };
                    let modified = metadata
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    all.push(DirEntry {
                        name: file_name,
                        is_dir,
                        location: EntryLocation::Fs(entry.path()),
                        size,
                        modified,
                    });
                }
            }
            let mut sorted: Vec<DirEntry> = Vec::new();
            if let Some(parent) = path_clone.parent() {
                sorted.push(DirEntry {
                    name: "..".to_string(),
                    is_dir: true,
                    location: EntryLocation::Fs(parent.to_path_buf()),
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
                        let metadata = entry.metadata().ok();
                        let size = if is_dir {
                            dir_sizes_fallback.get(&entry.path()).copied()
                        } else {
                            metadata.as_ref().map(|m| m.len())
                        };
                        let modified = metadata
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs());
                        all.push(DirEntry {
                            name: file_name,
                            is_dir,
                            location: EntryLocation::Fs(entry.path()),
                            size,
                            modified,
                        });
                    }
                }
            }
            let mut sorted: Vec<DirEntry> = Vec::new();
            if let Some(parent) = path_clone.parent() {
                sorted.push(DirEntry {
                    name: "..".to_string(),
                    is_dir: true,
                    location: EntryLocation::Fs(parent.to_path_buf()),
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
                    let _ = tx.send(DirBatch::Replace(batch));
                } else {
                    let _ = tx.send(DirBatch::Append(batch));
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
    let browser = &mut panel_state.browser;
    let initial_loading = initial.is_empty() || has_parent_entry;
    browser.current_path = path.clone();
    browser.browser_mode = BrowserMode::Fs;
    browser.entries = initial;
    browser.selected_index = 0;
    browser.top_index = 0;
    browser.inline_rename = None;
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
    cache_mode: ContainerLoadMode,
) {
    app.stash_container_cache(target_panel);
    let cache_key = (archive_path.clone(), cwd.clone(), kind);
    let mut cached = app.container_dir_cache.remove(&cache_key);
    if cache_mode == ContainerLoadMode::ForceReload {
        cached = None;
    }
    let mut initial: Vec<DirEntry> = if let Some(ref cache) = cached {
        cache.entries.clone()
    } else {
        Vec::new()
    };
    let cached_selection = cached
        .as_ref()
        .map(|cache| (cache.selected_index, cache.top_index));
    if initial.is_empty() {
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
                modified: None,
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
                modified: None,
            });
        }
    }

    let resume_rx = cached.as_mut().and_then(|cache| cache.entries_rx.take());
    let skip_loading = resume_rx.is_some() || cached.as_ref().is_some_and(|c| !c.loading);
    let (tx, rx) = mpsc::channel::<DirBatch>();
    let archive_clone = archive_path.clone();
    let cwd_clone = cwd.clone();
    let kind_clone = kind;
    let wake = app.wake.clone();

    if !skip_loading {
        if matches!(kind, ContainerKind::TarBz2 | ContainerKind::Tar) {
            thread::spawn(move || {
                let prefix = if cwd_clone.is_empty() {
                    "".to_string()
                } else {
                    format!("{}/", cwd_clone.trim_end_matches('/'))
                };
                let mut seen_dirs: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
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

                struct TarEmitContext<'a> {
                    implicit_prefix: Option<&'a str>,
                    cwd: &'a str,
                    kind: ContainerKind,
                    archive_path: &'a Path,
                    pending: &'a mut Vec<DirEntry>,
                    seen_dirs: &'a mut std::collections::HashSet<String>,
                    seen_files: &'a mut std::collections::HashSet<String>,
                    loaded: &'a mut usize,
                }

                fn emit_name(name: &str, ctx: &mut TarEmitContext<'_>) {
                    let rem = if let Some(prefix) = ctx.implicit_prefix {
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
                        if ctx.seen_dirs.insert(dir.clone()) {
                            ctx.pending.push(DirEntry {
                                name: dir.clone(),
                                is_dir: true,
                                location: EntryLocation::Container {
                                    kind: ctx.kind,
                                    archive_path: ctx.archive_path.to_path_buf(),
                                    inner_path: if let Some(prefix) = ctx.implicit_prefix {
                                        format!("{}{}", prefix, dir)
                                    } else if ctx.cwd.is_empty() {
                                        dir
                                    } else {
                                        format!("{}/{}", ctx.cwd.trim_end_matches('/'), dir)
                                    },
                                },
                                size: None,
                                modified: None,
                            });
                            *ctx.loaded += 1;
                        }
                    } else if ctx.seen_files.insert(rem.to_string()) {
                        let file_name = rem.to_string();
                        ctx.pending.push(DirEntry {
                            name: file_name.clone(),
                            is_dir: false,
                            location: EntryLocation::Container {
                                kind: ctx.kind,
                                archive_path: ctx.archive_path.to_path_buf(),
                                inner_path: if let Some(prefix) = ctx.implicit_prefix {
                                    format!("{}{}", prefix, file_name)
                                } else if ctx.cwd.is_empty() {
                                    file_name
                                } else {
                                    format!("{}/{}", ctx.cwd.trim_end_matches('/'), file_name)
                                },
                            },
                            size: None,
                            modified: None,
                        });
                        *ctx.loaded += 1;
                    }
                }

                let file = match std::fs::File::open(&archive_clone) {
                    Ok(file) => file,
                    Err(e) => {
                        let _ = tx.send(DirBatch::Error(format!("Failed to read archive: {e}")));
                        if let Some(ref wake) = wake {
                            wake();
                        }
                        return;
                    }
                };
                let reader = std::io::BufReader::new(file);
                let reader: Box<dyn Read> = match kind_clone {
                    ContainerKind::TarBz2 => Box::new(bzip2::read::BzDecoder::new(reader)),
                    ContainerKind::Tar => Box::new(reader),
                    _ => unreachable!(),
                };
                let mut archive = tar::Archive::new(reader);
                let entries = match archive.entries() {
                    Ok(entries) => entries,
                    Err(e) => {
                        let _ = tx.send(DirBatch::Error(format!("Failed to read archive: {e}")));
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
                                let mut ctx = TarEmitContext {
                                    implicit_prefix: root_ref.as_deref(),
                                    cwd: &cwd_clone,
                                    kind: kind_clone,
                                    archive_path: archive_clone.as_path(),
                                    pending: &mut pending,
                                    seen_dirs: &mut seen_dirs,
                                    seen_files: &mut seen_files,
                                    loaded: &mut loaded,
                                };
                                emit_name(&buffered_name, &mut ctx);
                            }
                        } else {
                            continue;
                        }
                    } else {
                        let root_ref = implicit_root
                            .as_ref()
                            .map(|root| format!("{}/", root.trim_end_matches('/')));
                        let emit_name_raw = if cwd_clone.is_empty() {
                            name.as_str()
                        } else {
                            rem
                        };
                        let mut ctx = TarEmitContext {
                            implicit_prefix: root_ref.as_deref(),
                            cwd: &cwd_clone,
                            kind: kind_clone,
                            archive_path: archive_clone.as_path(),
                            pending: &mut pending,
                            seen_dirs: &mut seen_dirs,
                            seen_files: &mut seen_files,
                            loaded: &mut loaded,
                        };
                        emit_name(emit_name_raw, &mut ctx);
                    }

                    if pending.len() >= BATCH || (!sent_first && !pending.is_empty()) {
                        let _ = tx.send(DirBatch::Append(pending));
                        pending = Vec::new();
                        sent_first = true;
                        let _ = tx.send(DirBatch::Progress {
                            loaded,
                            total: None,
                        });
                        if let Some(ref wake) = wake {
                            wake();
                        }
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
                        let mut ctx = TarEmitContext {
                            implicit_prefix: root_ref.as_deref(),
                            cwd: &cwd_clone,
                            kind: kind_clone,
                            archive_path: archive_clone.as_path(),
                            pending: &mut pending,
                            seen_dirs: &mut seen_dirs,
                            seen_files: &mut seen_files,
                            loaded: &mut loaded,
                        };
                        emit_name(&buffered_name, &mut ctx);
                    }
                }

                if !pending.is_empty() {
                    let _ = tx.send(DirBatch::Append(pending));
                    if let Some(ref wake) = wake {
                        wake();
                    }
                }
                let _ = tx.send(DirBatch::Progress {
                    loaded,
                    total: Some(loaded),
                });
                if let Some(ref wake) = wake {
                    wake();
                }
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
                        if let Some(ref wake) = wake {
                            wake();
                        }
                    },
                ) {
                    Ok(entries) => entries,
                    Err(e) => {
                        eprintln!("Failed to read container: {e}");
                        let _ = tx.send(DirBatch::Error(format!("Failed to read archive: {e}")));
                        if let Some(ref wake) = wake {
                            wake();
                        }
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
                    if let Some(ref wake) = wake {
                        wake();
                    }
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
                        if let Some(ref wake) = wake {
                            wake();
                        }
                        start = end;
                    }
                });
            });
        }
    }

    let remembered = prefer_name.clone().or_else(|| {
        app.container_last_selected_name
            .get(&(archive_path.clone(), cwd.clone(), kind))
            .cloned()
    });
    let panel_state = app.panel_mut(target_panel);
    let browser = &mut panel_state.browser;
    let initial_loading = cached
        .as_ref()
        .map(|cache| cache.loading)
        .unwrap_or(!skip_loading);

    browser.current_path = archive_path.clone();
    browser.browser_mode = BrowserMode::Container {
        kind,
        archive_path: archive_path.clone(),
        cwd: cwd.clone(),
    };
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
    browser.entries_rx = resume_rx.or(if skip_loading { None } else { Some(rx) });
    browser.prefer_select_name = remembered;
    browser.loading = initial_loading;
    browser.loading_progress = cached.and_then(|cache| cache.loading_progress);
}

fn open_selected(app: &mut AppState) {
    let active = app.active_panel;

    open_selected_from_to(app, active, active);
}

fn open_selected_external(app: &mut AppState) {
    if !app.allow_external_open {
        return;
    }
    let entry = {
        let panel = app.get_active_panel();
        let browser = &panel.browser;
        if browser.entries.is_empty() {
            return;
        }
        browser.entries[browser.selected_index].clone()
    };
    if let EntryLocation::Fs(path) = entry.location
        && let Err(err) = open_with_default_app(&path)
    {
        eprintln!("{err}");
    }
}

fn should_show_preview(app: &AppState, panel_side: ActivePanel) -> bool {
    let PanelState { mode, .. } = app.panel(panel_side);
    matches!(mode, PanelMode::Preview(_))
}

fn should_show_editor(app: &AppState, panel_side: ActivePanel) -> bool {
    let PanelState { mode, .. } = app.panel(panel_side);
    matches!(mode, PanelMode::Edit(_))
}

fn open_with_default_app(path: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", path.to_string_lossy().as_ref()])
            .spawn()
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("Failed to open with default app: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("Failed to open with default app: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("Failed to open with default app: {e}"))?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    Ok(())
}

fn open_selected_from_to(app: &mut AppState, source: ActivePanel, target: ActivePanel) {
    let (selected_entry, current_path, container_cwd) = {
        let panel = app.panel(source);
        let browser = &panel.browser;
        if browser.entries.is_empty() {
            return;
        }
        let entry = browser.entries[browser.selected_index].clone();
        let current_path = browser.current_path.clone();
        let BrowserState {
            browser_mode: ref mode,
            ..
        } = *browser;
        let container_cwd = match mode {
            BrowserMode::Container { cwd, .. } => Some(cwd.clone()),
            _ => None,
        };
        (entry, current_path, container_cwd)
    };

    app.store_selection_memory_for(source);
    app.push_history(target);

    match selected_entry.location.clone() {
        EntryLocation::Fs(path) => {
            if selected_entry.is_dir {
                let prefer_name = if selected_entry.name == ".." {
                    current_path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                } else {
                    None
                };
                load_fs_directory_async(app, path.clone(), target, prefer_name);

                if selected_entry.name != ".."
                    && let Some(name) = app.fs_last_selected_name.get(&path).cloned()
                {
                    app.select_entry_by_name(target, &name);
                }
            } else if let Some(kind) = container_kind_from_path(&path) {
                load_container_directory_async(
                    app,
                    kind,
                    path.clone(),
                    "".to_string(),
                    target,
                    None,
                    ContainerLoadMode::UseCache,
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
                    kind,
                    archive_path.clone(),
                    inner_path.clone(),
                    target,
                    prefer_name,
                    ContainerLoadMode::UseCache,
                );

                if selected_entry.name != ".."
                    && let Some(name) = app
                        .container_last_selected_name
                        .get(&(archive_path.clone(), inner_path.clone(), kind))
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
            let panel = app.panel_mut(side);
            let browser = &mut panel.browser;
            for entry in &mut browser.entries {
                if entry.is_dir
                    && let EntryLocation::Fs(p) = &entry.location
                    && *p == path
                {
                    entry.size = Some(size);
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
                ContainerLoadMode::UseCache,
            );
        }
        BrowserMode::Search { .. } => {
            let results = app.search_results.clone();
            let panel = app.panel_mut(which);
            let browser = &mut panel.browser;
            browser.browser_mode = snapshot.mode;
            browser.current_path = snapshot.current_path;
            browser.entries.clear();
            browser.entries.extend(results.iter().map(|result| {
                let BrowserState {
                    browser_mode: ref mode,
                    ..
                } = *browser;
                let display_name = match mode {
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
        let browser = &panel.browser;
        browser.current_path.clone()
    };
    {
        app.push_history(app.active_panel);
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
    if app.props_dialog.is_some() {
        if input.key_pressed(egui::Key::Escape) {
            app.props_dialog = None;
            ctx.request_repaint();
        }
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
    if handle_inline_rename(app, input) {
        ctx.request_repaint();
        return;
    }
    if let PanelMode::Edit(ref mut edit) = app.panel_mut(app.active_panel).mode {
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
                let return_focus = edit.return_focus;
                let panel = app.panel_mut(app.active_panel);
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
                let return_focus = edit.return_focus;
                let panel = app.panel_mut(app.active_panel);
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
                let return_focus = edit.return_focus;
                let panel = app.panel_mut(app.active_panel);
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
    let alt_enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::Enter));
    if alt_enter {
        open_props_dialog(app);
        ctx.request_repaint();
        return;
    }
    let shift_enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter));
    if shift_enter {
        open_selected_external(app);
        ctx.request_repaint();
        return;
    }
    if input.key_pressed(egui::Key::Escape) {
        let panel = app.get_active_panel();
        if matches!(panel.browser.browser_mode, BrowserMode::Search { .. }) {
            cancel_search(app);
            if let Some(snapshot) = app.pop_history_back(app.active_panel) {
                apply_panel_snapshot(app, app.active_panel, snapshot);
            }
            ctx.request_repaint();
            return;
        }
    }
    let window_rows = active_window_rows(app, cache);
    let tab_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
    let ctrl_tab_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::I));
    if tab_pressed || ctrl_tab_pressed {
        if app.props_dialog.is_none() {
            app.switch_panel();
        }
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
            let browser = &panel.browser;
            browser
                .entries
                .get(browser.selected_index)
                .and_then(|entry| {
                    if entry.is_dir
                        && let EntryLocation::Fs(path) = &entry.location
                    {
                        return Some(path.clone());
                    }
                    None
                })
        };
        if let Some(path) = target_path
            && !app.dir_size_pending.contains(&path)
        {
            app.dir_size_pending.insert(path.clone());
            let _ = app.dir_size_tx.send(path);
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
    if alt_left && let Some(snapshot) = app.pop_history_back(app.active_panel) {
        apply_panel_snapshot(app, app.active_panel, snapshot);
    }
    let alt_right = ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight));
    if alt_right && let Some(snapshot) = app.pop_history_forward(app.active_panel) {
        apply_panel_snapshot(app, app.active_panel, snapshot);
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
            if matches!(
                app.get_active_panel().browser.browser_mode,
                BrowserMode::Search { .. }
            ) {
                // Open selected result.
            } else {
                start_search(app);
            }
        }
        if matches!(
            app.get_active_panel().browser.browser_mode,
            BrowserMode::Search { .. }
        ) {
            app.push_history(app.active_panel);
            let panel = app.get_active_panel();
            let browser = &panel.browser;
            let entry = browser.entries.get(browser.selected_index).cloned();
            if let Some(entry) = entry
                && let EntryLocation::Fs(path) = entry.location
            {
                if entry.is_dir {
                    load_fs_directory_async(app, path, app.active_panel, None);
                } else if let Some(parent) = path.parent() {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string());
                    load_fs_directory_async(app, parent.to_path_buf(), app.active_panel, name);
                }
            }
            app.search_ui = SearchUiState::Closed;
        } else if app.theme_picker_open {
            app.apply_selected_theme();
        } else {
            open_selected(app);
        }
    }
    if let PanelMode::Preview(ref mut preview) = app.panel_mut(app.active_panel).mode {
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
            preview.scroll += page * 10.0;
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
        if let PanelMode::Preview(ref mut preview) = app.panel_mut(app.active_panel).mode {
            preview.find_open = true;
            preview.find_focus = true;
        }
        ctx.request_repaint();
    }
    let PanelState {
        mode: active_mode, ..
    } = app.panel(app.active_panel);
    let active_is_browser = matches!(active_mode, PanelMode::Browser);
    if input.key_pressed(egui::Key::ArrowDown) && active_is_browser {
        if app.theme_picker_open {
            app.select_next_theme();
        } else {
            let browser = &app.get_active_panel().browser;
            if browser.selected_index + 1 < browser.entries.len() {
                app.select_entry(browser.selected_index + 1, window_rows);
            }
        }
    }
    if input.key_pressed(egui::Key::ArrowUp) && active_is_browser {
        if app.theme_picker_open {
            app.select_prev_theme();
        } else {
            let browser = &app.get_active_panel().browser;
            if browser.selected_index > 0 {
                app.select_entry(browser.selected_index - 1, window_rows);
            }
        }
    }
    if input.key_pressed(egui::Key::PageUp) && active_is_browser {
        let browser = &app.get_active_panel().browser;
        let new_index = browser.selected_index.saturating_sub(window_rows);
        app.select_entry(new_index, window_rows);
    }
    if input.key_pressed(egui::Key::PageDown) && active_is_browser {
        let browser = &app.get_active_panel().browser;
        let len = browser.entries.len();
        let mut new_index = browser.selected_index.saturating_add(window_rows);
        if len > 0 && new_index >= len {
            new_index = len - 1;
        }
        app.select_entry(new_index, window_rows);
    }
    if input.key_pressed(egui::Key::Home) && active_is_browser {
        app.select_entry(0, window_rows);
    }
    if input.key_pressed(egui::Key::End) && active_is_browser {
        let browser = &app.get_active_panel().browser;
        if !browser.entries.is_empty() {
            app.select_entry(browser.entries.len() - 1, window_rows);
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
    let other_panel_preview = app
        .preview_panel_side()
        .is_some_and(|side| side != app.active_panel);
    if input.key_pressed(egui::Key::F5) && !other_panel_preview {
        app.prepare_copy_selected();
    }
    if input.key_pressed(egui::Key::F6) && !other_panel_preview {
        app.prepare_move_selected();
    }
    if input.key_pressed(egui::Key::F4) {
        app.prepare_edit_selected();
        ctx.request_repaint();
    }
    let shift_f4 = ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::F4));
    if shift_f4 {
        app.start_inline_new_file();
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
    let browser = &panel.browser;
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
            let panel = app.get_active_panel();
            let browser = &panel.browser;
            let mut next_name: Option<String> = None;
            if !browser.entries.is_empty() {
                let mut next_idx = browser.selected_index.saturating_add(1);
                while next_idx < browser.entries.len() {
                    let candidate = &browser.entries[next_idx].name;
                    if candidate != ".." {
                        next_name = Some(candidate.clone());
                        break;
                    }
                    next_idx += 1;
                }
                if next_name.is_none() {
                    let mut prev_idx = browser.selected_index.saturating_sub(1);
                    while prev_idx < browser.entries.len() {
                        let candidate = &browser.entries[prev_idx].name;
                        if candidate != ".." {
                            next_name = Some(candidate.clone());
                            break;
                        }
                        if prev_idx == 0 {
                            break;
                        }
                        prev_idx -= 1;
                    }
                }
            }
            let parent = target.parent().unwrap_or_else(|| std::path::Path::new("."));
            if let Some(next_name) = next_name {
                app.fs_last_selected_name
                    .insert(parent.to_path_buf(), next_name);
            } else {
                app.fs_last_selected_name.remove(parent);
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
            if let Some(current) = src.file_name().and_then(|n| n.to_str())
                && current == name
            {
                app.clear_pending_op();
                return;
            }
            app.store_selection_memory_for(app.active_panel);
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
    let which = app.active_panel;
    let panel = app.panel(which);
    let browser = &panel.browser;
    let path = browser.current_path.clone();
    if matches!(browser.browser_mode, BrowserMode::Fs) {
        load_fs_directory_async(app, path, which, None);
    }
}

fn refresh_fs_panels(app: &mut AppState) {
    for which in [ActivePanel::Left, ActivePanel::Right] {
        let browser = &app.panel(which).browser;
        if !matches!(browser.browser_mode, BrowserMode::Fs) {
            continue;
        }
        let path = browser.current_path.clone();
        load_fs_directory_async(app, path, which, None);
    }
}

fn reload_panel(app: &mut AppState, which: ActivePanel) {
    let (mode, current_path, selected_name) = {
        let panel = app.panel(which);
        let browser = &panel.browser;
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
        BrowserMode::Fs => load_fs_directory_async(app, current_path, which, selected_name),
        BrowserMode::Container {
            kind,
            archive_path,
            cwd,
        } => load_container_directory_async(
            app,
            kind,
            archive_path,
            cwd,
            which,
            selected_name,
            ContainerLoadMode::ForceReload,
        ),
        BrowserMode::Search { .. } => {
            let results = app.search_results.clone();
            let panel = app.panel_mut(which);
            let browser = &mut panel.browser;
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

fn draw_confirmation(ctx: &egui::Context, app: &mut AppState) {
    let op = match app.pending_op.clone() {
        Some(op) => op,
        None => return,
    };
    let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));

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

    if !is_rename {
        if enter {
            confirmed = true;
        }
        if escape {
            cancelled = true;
        }
    }

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
        let panel = app.panel_mut(side);
        if accept {
            let return_focus = match panel.mode {
                PanelMode::Edit(ref edit) => edit.return_focus,
                _ => side,
            };
            panel.mode = PanelMode::Browser;
            app.active_panel = return_focus;
        } else if let PanelMode::Edit(ref mut edit) = panel.mode {
            edit.confirm_discard = false;
        }
    }
}

fn open_props_dialog(app: &mut AppState) {
    let panel = app.get_active_panel();
    let browser = &panel.browser;
    if !matches!(browser.browser_mode, BrowserMode::Fs) {
        return;
    }
    if browser.entries.is_empty() {
        return;
    }
    let entry = &browser.entries[browser.selected_index];
    if entry.name == ".." {
        return;
    }
    let EntryLocation::Fs(path) = &entry.location else {
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
    let user_label = get_user_by_uid(uid)
        .map(|user| user.name().to_string_lossy().into_owned())
        .unwrap_or_else(|| uid.to_string());
    let group_label = get_group_by_gid(gid)
        .map(|group| group.name().to_string_lossy().into_owned())
        .unwrap_or_else(|| gid.to_string());

    app.props_dialog = Some(PropsDialog {
        target: path.clone(),
        original: FileProps {
            mode,
            uid,
            gid,
            file_type,
            is_dir,
            user_label: user_label.clone(),
            group_label: group_label.clone(),
        },
        current: FilePropsEdit {
            mode: mode & 0o777,
            user: user_label,
            group: group_label,
        },
        error: None,
    });
}

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

fn parse_user_value(input: &str) -> Result<u32, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Owner user is required".to_string());
    }
    if let Ok(uid) = trimmed.parse::<u32>() {
        return Ok(uid);
    }
    get_user_by_name(trimmed)
        .map(|user| user.uid())
        .ok_or_else(|| format!("Unknown user: {trimmed}"))
}

fn parse_group_value(input: &str) -> Result<u32, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Owner group is required".to_string());
    }
    if let Ok(gid) = trimmed.parse::<u32>() {
        return Ok(gid);
    }
    get_group_by_name(trimmed)
        .map(|group| group.gid())
        .ok_or_else(|| format!("Unknown group: {trimmed}"))
}

fn draw_props_modal(ctx: &egui::Context, app: &mut AppState) {
    let Some(dialog) = app.props_dialog.as_mut() else {
        return;
    };
    let colors = app.theme.colors();
    let screen = ctx.available_rect();
    let overlay_layer = egui::LayerId::new(egui::Order::Foreground, "props_overlay".into());
    ctx.layer_painter(overlay_layer).rect_filled(
        screen,
        egui::CornerRadius::ZERO,
        egui::Color32::from_black_alpha(140),
    );

    let original_perms = dialog.original.mode & 0o777;
    let user_changed = dialog.current.user.trim() != dialog.original.user_label;
    let group_changed = dialog.current.group.trim() != dialog.original.group_label;
    let changed_color = color32(colors.row_fg_selected);
    let normal_color = color32(colors.row_fg_active);

    let mut action: Option<(&'static str, bool)> = None;
    let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let tab_pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
    if escape {
        app.props_dialog = None;
        return;
    }

    if tab_pressed {
        if ctx.memory(|mem| mem.has_focus(egui::Id::new("props_owner_user"))) {
            ctx.memory_mut(|mem| mem.request_focus(egui::Id::new("props_owner_group")));
        } else {
            ctx.memory_mut(|mem| mem.request_focus(egui::Id::new("props_owner_user")));
        }
    }

    egui::Window::new("Properties")
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::Vec2::new(480.0, 300.0))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(dialog.target.to_string_lossy());
            ui.add_space(6.0);
            egui::Grid::new("props_grid")
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.colored_label(color32(colors.row_fg_inactive), "Type");
                    ui.colored_label(color32(colors.row_fg_active), &dialog.original.file_type);
                    ui.end_row();
                    ui.colored_label(
                        if user_changed {
                            changed_color
                        } else {
                            normal_color
                        },
                        "Owner (user)",
                    );
                    let user_response = ui.add(
                        egui::TextEdit::singleline(&mut dialog.current.user)
                            .desired_width(220.0)
                            .id(egui::Id::new("props_owner_user")),
                    );
                    ui.end_row();
                    ui.colored_label(
                        if group_changed {
                            changed_color
                        } else {
                            normal_color
                        },
                        "Owner (group)",
                    );
                    let group_response = ui.add(
                        egui::TextEdit::singleline(&mut dialog.current.group)
                            .desired_width(220.0)
                            .id(egui::Id::new("props_owner_group")),
                    );
                    if user_response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Tab)) {
                        group_response.request_focus();
                    }
                    ui.end_row();
                    ui.colored_label(normal_color, "Permissions");
                    ui.vertical(|ui| {
                        let perm_colors = PermRowColors {
                            original_mode: original_perms,
                            changed_color,
                            normal_color,
                        };
                        egui::Grid::new("perm_grid")
                            .spacing([8.0, 4.0])
                            .show(ui, |ui| {
                                ui.colored_label(color32(colors.row_fg_inactive), "");
                                ui.label("Read");
                                ui.label("Write");
                                ui.label("Exec");
                                ui.end_row();
                                perms_row(
                                    ui,
                                    "User",
                                    0o400,
                                    0o200,
                                    0o100,
                                    &mut dialog.current.mode,
                                    perm_colors,
                                );
                                perms_row(
                                    ui,
                                    "Group",
                                    0o040,
                                    0o020,
                                    0o010,
                                    &mut dialog.current.mode,
                                    perm_colors,
                                );
                                perms_row(
                                    ui,
                                    "Other",
                                    0o004,
                                    0o002,
                                    0o001,
                                    &mut dialog.current.mode,
                                    perm_colors,
                                );
                            });
                    });
                    ui.end_row();
                    if user_response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Tab)) {
                        group_response.request_focus();
                    }
                });
            if let Some(error) = dialog.error.as_ref() {
                ui.add_space(6.0);
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let apply = ui.add(egui::Button::new("Apply").min_size(egui::vec2(110.0, 0.0)));
                if dialog.original.is_dir {
                    let recursive =
                        ui.add(egui::Button::new("Recursive").min_size(egui::vec2(130.0, 0.0)));
                    if recursive.clicked() {
                        action = Some(("apply", true));
                    }
                }
                let cancel = ui.add(egui::Button::new("Cancel").min_size(egui::vec2(110.0, 0.0)));
                if apply.clicked() || enter {
                    action = Some(("apply", false));
                }
                if cancel.clicked() {
                    action = Some(("cancel", false));
                }
            });
        });

    if let Some((what, recursive)) = action {
        match what {
            "apply" => apply_props_dialog(app, recursive),
            "cancel" => app.props_dialog = None,
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
struct PermRowColors {
    original_mode: u32,
    changed_color: Color32,
    normal_color: Color32,
}

fn perms_row(
    ui: &mut egui::Ui,
    label: &str,
    read_bit: u32,
    write_bit: u32,
    exec_bit: u32,
    mode: &mut u32,
    colors: PermRowColors,
) {
    let row_mask = read_bit | write_bit | exec_bit;
    let changed = (*mode & row_mask) != (colors.original_mode & row_mask);
    ui.colored_label(
        if changed {
            colors.changed_color
        } else {
            colors.normal_color
        },
        label,
    );
    let mut r = *mode & read_bit != 0;
    let mut w = *mode & write_bit != 0;
    let mut x = *mode & exec_bit != 0;
    if ui.checkbox(&mut r, "").changed() {
        if r {
            *mode |= read_bit;
        } else {
            *mode &= !read_bit;
        }
    }
    if ui.checkbox(&mut w, "").changed() {
        if w {
            *mode |= write_bit;
        } else {
            *mode &= !write_bit;
        }
    }
    if ui.checkbox(&mut x, "").changed() {
        if x {
            *mode |= exec_bit;
        } else {
            *mode &= !exec_bit;
        }
    }
    ui.end_row();
}

fn apply_props_dialog(app: &mut AppState, recursive: bool) {
    let Some(dialog) = app.props_dialog.as_mut() else {
        return;
    };
    dialog.error = None;
    let uid = match parse_user_value(&dialog.current.user) {
        Ok(uid) => uid,
        Err(err) => {
            dialog.error = Some(err);
            return;
        }
    };
    let gid = match parse_group_value(&dialog.current.group) {
        Ok(gid) => gid,
        Err(err) => {
            dialog.error = Some(err);
            return;
        }
    };
    let new_mode = (dialog.original.mode & !0o777) | (dialog.current.mode & 0o777);
    let changed = new_mode != dialog.original.mode
        || uid != dialog.original.uid
        || gid != dialog.original.gid;
    if !changed {
        app.props_dialog = None;
        return;
    }
    if let Err(e) = app.io_tx.send(fileman::core::IOTask::SetProps {
        path: dialog.target.clone(),
        mode: new_mode,
        uid,
        gid,
        recursive,
    }) {
        dialog.error = Some(format!("Failed to queue update: {e}"));
        return;
    }
    app.io_in_flight = app.io_in_flight.saturating_add(1);
    app.props_dialog = None;
    app.store_selection_memory_for(app.active_panel);
    refresh_active_panel(app);
}

fn make_whitespace_visible(text: &str) -> String {
    text.replace('\t', "→   ")
        .lines()
        .map(|line| format!("{line}⏎"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn handle_inline_rename(app: &mut AppState, input: &egui::InputState) -> bool {
    let enter = input.key_pressed(egui::Key::Enter);
    let escape = input.key_pressed(egui::Key::Escape);
    let (action, next_selection, handled) = {
        let panel = app.get_active_panel_mut();
        let browser = &mut panel.browser;
        let Some(_rename) = browser.inline_rename.as_ref() else {
            return false;
        };
        if !enter && !escape {
            return true;
        }
        let rename = browser.inline_rename.take().unwrap();
        if escape {
            if rename.new_file && rename.index < browser.entries.len() {
                browser.entries.remove(rename.index);
                if browser.selected_index >= browser.entries.len() && !browser.entries.is_empty() {
                    browser.selected_index = browser.entries.len() - 1;
                }
            }
            return true;
        }
        let new_name = rename.text.trim();
        if new_name.is_empty()
            || new_name == "."
            || new_name == ".."
            || new_name.contains('/')
            || new_name.contains('\\')
        {
            return true;
        }
        let mut action: Option<fileman::core::IOTask> = None;
        let mut next_selection: Option<(PathBuf, String)> = None;
        if rename.new_file {
            let dir = browser.current_path.clone();
            let path = dir.join(new_name);
            action = Some(fileman::core::IOTask::WriteFile {
                path,
                contents: Vec::new(),
            });
            next_selection = Some((dir, new_name.to_string()));
        } else if rename.index < browser.entries.len() {
            let entry = &browser.entries[rename.index];
            if let EntryLocation::Fs(path) = &entry.location {
                let current = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if current != new_name {
                    action = Some(fileman::core::IOTask::Rename {
                        src: path.clone(),
                        new_name: new_name.to_string(),
                    });
                    next_selection = Some((
                        path.parent()
                            .unwrap_or_else(|| std::path::Path::new("."))
                            .to_path_buf(),
                        new_name.to_string(),
                    ));
                }
            }
        }
        (action, next_selection, true)
    };
    if !handled {
        return false;
    }
    if let Some(task) = action {
        let _ = app.io_tx.send(task);
        app.io_in_flight = app.io_in_flight.saturating_add(1);
        if let Some((dir, name)) = next_selection {
            app.fs_last_selected_name.insert(dir, name);
        }
        refresh_active_panel(app);
    }
    true
}

struct EditorRender<'a> {
    theme: &'a Theme,
    is_focused: bool,
    edit: &'a mut EditState,
    highlight_cache: &'a HashMap<String, egui::text::LayoutJob>,
    highlight_pending: &'a mut HashSet<String>,
    highlight_req_tx: &'a mpsc::Sender<HighlightRequest>,
    min_height: f32,
}

fn draw_editor(ui: &mut egui::Ui, ctx: EditorRender<'_>) {
    let EditorRender {
        theme,
        is_focused,
        edit,
        highlight_cache,
        highlight_pending,
        highlight_req_tx,
        min_height,
    } = ctx;
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
            if key.is_none()
                && let Some(path) = edit.path.as_ref()
            {
                key = Some(format!("edit:{}", path.to_string_lossy()));
                edit.highlight_key = key.clone();
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
            let mut edit_output: Option<TextEditOutput> = None;
            egui::ScrollArea::vertical()
                .id_salt("editor_scroll")
                .auto_shrink([false, false])
                .max_height(editor_height)
                .show(ui, |ui| {
                    let output = egui::TextEdit::multiline(&mut text)
                        .font(egui::TextStyle::Monospace)
                        .layouter(&mut layouter)
                        .code_editor()
                        .cursor_at_end(false)
                        .id_source("editor_text")
                        .desired_width(f32::INFINITY)
                        .desired_rows(desired_rows)
                        .show(ui);
                    edit_output = Some(output);
                });
            let response = edit_output
                .as_ref()
                .map(|output| output.response.clone())
                .unwrap_or_else(|| ui.label(" "));
            edit.text = text;
            if response.changed() {
                edit.highlight_hash = hash_text(&edit.text);
                edit.highlight_wrap_width = 0.0;
                edit.highlight_dirty_at = Some(std::time::Instant::now());
                edit.dirty = true;
            }
            if needs_highlight && let Some(key) = edit.highlight_key.clone() {
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
                ui.add_space(6.0);
                let (row, col) = edit_output
                    .as_ref()
                    .and_then(|output| output.cursor_range)
                    .map(|range| range.primary.index)
                    .map(|idx| cursor_row_col(&edit.text, idx))
                    .unwrap_or((1, 1));
                let info = format!("row: {row}, col: {col}");
                let font = egui::TextStyle::Body.resolve(ui.style());
                let width = ui
                    .painter()
                    .layout_no_wrap(info.clone(), font.clone(), color32(colors.row_fg_inactive))
                    .size()
                    .x;
                let rect = ui.available_rect_before_wrap();
                let pos = egui::pos2(rect.right() - width, rect.center().y);
                ui.painter().text(
                    pos,
                    egui::Align2::LEFT_CENTER,
                    info,
                    font,
                    color32(colors.row_fg_inactive),
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

struct PreviewRender<'a> {
    theme: &'a Theme,
    is_focused: bool,
    preview: &'a mut PreviewState,
    image_cache: &'a mut ImageCache,
    image_req_tx: &'a mpsc::Sender<ImageRequest>,
    highlight_cache: &'a HashMap<String, egui::text::LayoutJob>,
    highlight_pending: &'a mut HashSet<String>,
    highlight_req_tx: &'a mpsc::Sender<HighlightRequest>,
    min_height: f32,
}

fn draw_preview(ui: &mut egui::Ui, ctx: PreviewRender<'_>) {
    let PreviewRender {
        theme,
        is_focused,
        preview,
        image_cache,
        image_req_tx,
        highlight_cache,
        highlight_pending,
        highlight_req_tx,
        min_height,
    } = ctx;
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

            if let Some(PreviewContent::Text(_)) = preview.content.as_ref() {
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
                ui.horizontal(|ui| {
                    ui.checkbox(&mut preview.wrap, "Wrap");
                    ui.checkbox(&mut preview.show_whitespace, "Show whitespace");
                });
                ui.add_space(6.0);
            } else if let Some(PreviewContent::Binary(_)) = preview.content.as_ref() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut preview.bytes_per_row_auto, "Auto bytes/row");
                    if !preview.bytes_per_row_auto {
                        ui.add(
                            egui::Slider::new(&mut preview.bytes_per_row, 4..=32)
                                .step_by(4.0)
                                .text("bytes/row"),
                        );
                    }
                });
                ui.add_space(6.0);
            }

            let page_height = ui.available_height();
            let output = ui
                .scope_builder(
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        ui.available_rect_before_wrap().min,
                        egui::Vec2::new(ui.available_width(), page_height),
                    )),
                    |ui| {
                        let scroll = if preview.wrap {
                            egui::ScrollArea::vertical()
                        } else {
                            egui::ScrollArea::both()
                        };
                        scroll
                            .auto_shrink([false, false])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                            )
                            .vertical_scroll_offset(preview.scroll)
                            .show(ui, |ui| match preview.content.as_ref() {
                                Some(PreviewContent::Text(text)) => {
                                    let display_text = if preview.show_whitespace {
                                        make_whitespace_visible(text)
                                    } else {
                                        text.clone()
                                    };

                                    let ext = preview.ext.clone();
                                    let base_key = preview
                                        .key
                                        .clone()
                                        .unwrap_or_else(|| "unknown".to_string());
                                    let key = format!("{base_key}:{:x}", hash_text(&display_text));
                                    let wrap_width = if preview.wrap {
                                        ui.available_width()
                                    } else {
                                        f32::INFINITY
                                    };
                                    if let Some(job) = highlight_cache.get(&key) {
                                        let mut job = job.clone();
                                        job.wrap.max_width = wrap_width;
                                        job.wrap.break_anywhere = preview.wrap;
                                        let label = egui::Label::new(job)
                                            .selectable(true)
                                            .wrap_mode(if preview.wrap {
                                                egui::TextWrapMode::Wrap
                                            } else {
                                                egui::TextWrapMode::Extend
                                            });
                                        ui.add(label);
                                    } else {
                                        ui.horizontal(|ui| {
                                            ui.add(egui::Spinner::new());
                                            ui.colored_label(text_color, "Highlighting…");
                                        });
                                        ui.add_space(6.0);
                                        if highlight_pending.insert(key.clone()) {
                                            let _ = highlight_req_tx.send(HighlightRequest {
                                                key: key.clone(),
                                                text: display_text.clone(),
                                                ext,
                                                theme_kind: theme.kind,
                                            });
                                        }
                                        let mut job = egui::text::LayoutJob::simple(
                                            display_text.clone(),
                                            egui::TextStyle::Monospace.resolve(ui.style()),
                                            egui::Color32::LIGHT_GRAY,
                                            wrap_width,
                                        );
                                        job.wrap.break_anywhere = preview.wrap;
                                        let label = egui::Label::new(job)
                                            .selectable(true)
                                            .wrap_mode(if preview.wrap {
                                                egui::TextWrapMode::Wrap
                                            } else {
                                                egui::TextWrapMode::Extend
                                            });
                                        ui.add(label);
                                    }
                                }
                                Some(PreviewContent::Binary(bytes)) => {
                                    let width = if preview.bytes_per_row_auto {
                                        let mut best = 4usize;
                                        let options = [4usize, 8, 12, 16, 20, 24, 28, 32];
                                        let font = egui::TextStyle::Monospace.resolve(ui.style());
                                        for opt in options {
                                            let sample = hexdump_with_width(
                                                &bytes[..bytes.len().min(opt)],
                                                opt,
                                            );
                                            let sample = sample.lines().next().unwrap_or_default();
                                            let w = ui
                                                .painter()
                                                .layout_no_wrap(
                                                    sample.to_string(),
                                                    font.clone(),
                                                    text_color,
                                                )
                                                .size()
                                                .x;
                                            if w <= ui.available_width() {
                                                best = opt;
                                            } else {
                                                break;
                                            }
                                        }
                                        preview.bytes_per_row = best;
                                        best
                                    } else {
                                        preview.bytes_per_row
                                    };
                                    let job = hexdump_job(bytes, width, &colors, ui);
                                    ui.add(egui::Label::new(job).selectable(true));
                                }
                                Some(PreviewContent::Image(path)) => {
                                    let (key, request) = match path {
                                        ImageLocation::Fs(path) => {
                                            let key = path.to_string_lossy().into_owned();
                                            (
                                                key.clone(),
                                                ImageRequest {
                                                    key,
                                                    source: ImageSource::Fs(
                                                        path.as_ref().to_path_buf(),
                                                    ),
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
                                                    ContainerKind::Tar => "tar",
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
                                    if let Some(message) = image_cache.failures.get(&key) {
                                        ui.colored_label(
                                            text_color,
                                            format!("Failed to decode image\n{message}"),
                                        );
                                    } else if let Some(handle) =
                                        image_cache.textures.get(&key).cloned()
                                    {
                                        touch_image(image_cache, &key);
                                        if let Some(meta) = image_cache.meta.get(&key) {
                                            let depth_bits = match meta.depth {
                                                BitDepth::Eight => "8-bit",
                                                BitDepth::Sixteen => "16-bit",
                                                BitDepth::Float32 => "32-bit",
                                                _ => "unknown",
                                            };
                                            ui.colored_label(
                                                text_color,
                                                format!(
                                                    "{}×{} · {}",
                                                    meta.width, meta.height, depth_bits
                                                ),
                                            );
                                            ui.add_space(6.0);
                                        }
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
                                        ui.colored_label(
                                            text_color,
                                            format!("Loading image...\n{}", key),
                                        );
                                        ui.ctx().request_repaint_after(
                                            std::time::Duration::from_millis(120),
                                        );
                                    }
                                }
                                None => {
                                    ui.colored_label(text_color, "No preview");
                                }
                            })
                    },
                )
                .inner;
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

fn draw_command_bar(ctx: &egui::Context, app: &AppState, colors: &ThemeColors) {
    let modifiers = ctx.input(|i| i.modifiers);
    let preview_side = app.preview_panel_side();
    let other_panel_preview = preview_side
        .as_ref()
        .is_some_and(|side| *side != app.active_panel);
    egui::TopBottomPanel::bottom("command_bar")
        .exact_height(30.0)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(color32(colors.footer_bg))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (mut f3, f4, mut f5, mut f6, f7, f8) = if modifiers.alt {
                            ("", "", "Pack", "Unpack", "Search", "Command")
                        } else if modifiers.shift {
                            ("", "New", "Copy", "Rename", "", "")
                        } else {
                            ("View", "Edit", "Copy", "Move", "Mkdir", "Delete")
                        };
                        if preview_side.is_some() && !modifiers.alt && !modifiers.shift {
                            f3 = "Exit";
                        }
                        if other_panel_preview {
                            if !f5.is_empty() {
                                f5 = "";
                            }
                            if !f6.is_empty() {
                                f6 = "";
                            }
                            if modifiers.shift {
                                f5 = "";
                            }
                        }
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

    let (
        mut entries_len,
        mut selected_index,
        header_text,
        mut selected_label,
        mut loading,
        mut loading_progress,
    ) = {
        let panel = app.panel(panel_side);
        let browser = &panel.browser;
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
    let panel_side_for_closure = panel_side;

    let mut request_raw_reload = false;
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
                                let panel = app.panel_mut(panel_side);
                                let browser = &mut panel.browser;
                                let mut sort_mode = browser.sort_mode;
                                let mut sort_desc = browser.sort_desc;
                                let mut sort_changed = false;
                                let previous_sort_mode = browser.sort_mode;

                                let full_width = ui.available_width();
                                let controls_width = 120.0;
                                let gap = 24.0;
                                let left_width = (full_width - controls_width - gap).max(0.0);
                                let prev_spacing = ui.spacing().item_spacing;
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.horizontal(|ui| {
                                    let (left_rect, _) = ui.allocate_exact_size(
                                        egui::Vec2::new(left_width, ui.available_height()),
                                        egui::Sense::hover(),
                                    );
                                    let header_display = if is_active {
                                        format!("● {header_text}")
                                    } else {
                                        header_text.clone()
                                    };
                                    let header_font = egui::TextStyle::Body.resolve(ui.style());
                                    ui.painter().with_clip_rect(left_rect).text(
                                        left_rect.left_center(),
                                        egui::Align2::LEFT_CENTER,
                                        header_display,
                                        header_font,
                                        color32(colors.header_fg),
                                    );
                                    if left_width > 0.0 {
                                        ui.add_space(gap);
                                    }
                                    ui.allocate_ui_with_layout(
                                        egui::Vec2::new(controls_width, ui.available_height()),
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            egui::ComboBox::from_id_salt(match panel_side {
                                                ActivePanel::Left => "left_sort_mode",
                                                ActivePanel::Right => "right_sort_mode",
                                            })
                                            .selected_text(sort_mode_label(sort_mode))
                                            .show_ui(
                                                ui,
                                                |ui| {
                                                    sort_changed |= ui
                                                        .selectable_value(
                                                            &mut sort_mode,
                                                            SortMode::Name,
                                                            "Name",
                                                        )
                                                        .changed();
                                                    sort_changed |= ui
                                                        .selectable_value(
                                                            &mut sort_mode,
                                                            SortMode::Date,
                                                            "Date",
                                                        )
                                                        .changed();
                                                    sort_changed |= ui
                                                        .selectable_value(
                                                            &mut sort_mode,
                                                            SortMode::Size,
                                                            "Size",
                                                        )
                                                        .changed();
                                                    sort_changed |= ui
                                                        .selectable_value(
                                                            &mut sort_mode,
                                                            SortMode::Raw,
                                                            "Raw",
                                                        )
                                                        .changed();
                                                },
                                            );
                                            let arrow = if sort_desc { "v" } else { "^" };
                                            if ui.small_button(arrow).clicked() {
                                                sort_desc = !sort_desc;
                                                sort_changed = true;
                                            }
                                        },
                                    );
                                });
                                ui.spacing_mut().item_spacing = prev_spacing;

                                if sort_changed {
                                    browser.sort_mode = sort_mode;
                                    browser.sort_desc = sort_desc;
                                    if sort_mode == SortMode::Raw
                                        && previous_sort_mode != SortMode::Raw
                                    {
                                        request_raw_reload = true;
                                    } else {
                                        resort_browser_entries(browser);
                                    }
                                }
                            });
                    },
                );

                if request_raw_reload {
                    reload_panel(app, panel_side);
                    let panel = app.panel(panel_side);
                    let browser = &panel.browser;
                    entries_len = browser.entries.len();
                    selected_index = browser.selected_index.min(entries_len.saturating_sub(1));
                    selected_label = browser
                        .entries
                        .get(selected_index)
                        .map(|e| e.name.clone())
                        .unwrap_or_else(|| "-".to_string());
                    loading = browser.loading;
                    loading_progress = browser.loading_progress;
                }

                if loading {
                    let progress = loading_progress.unwrap_or((0, None));
                    let ratio = match progress.1 {
                        Some(total) if total > 0 => progress.0 as f32 / total as f32,
                        _ => 0.0,
                    };
                    ui.add_space(4.0);
                    let loading_label = matches!(
                        app.panel(panel_side).browser.browser_mode,
                        BrowserMode::Search { .. }
                    );
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
                                let (entry, rename_active) = {
                                    let browser = &app.panel(panel_side_for_closure).browser;
                                    let entry = browser.entries[idx].clone();
                                    let rename_active = browser
                                        .inline_rename
                                        .as_ref()
                                        .is_some_and(|rename| rename.index == idx);
                                    (entry, rename_active)
                                };
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
                                let file_tint = if entry.is_dir {
                                    None
                                } else if is_text_name(&entry.name) {
                                    Some(Color::rgba(0.22, 0.78, 0.56, 1.0))
                                } else if is_media_name(&entry.name) {
                                    Some(Color::rgba(0.32, 0.68, 1.0, 1.0))
                                } else {
                                    Some(Color::rgba(0.92, 0.68, 0.28, 1.0))
                                };
                                if !is_selected && let Some(tint) = file_tint {
                                    let factor = if is_active { 0.42 } else { 0.32 };
                                    fg = blend_color(fg, tint, factor);
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
                                } else if is_selected {
                                    fg
                                } else if let Some(tint) = file_tint {
                                    blend_color(fg, tint, 0.85)
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
                                if size_text.is_empty()
                                    && entry.is_dir
                                    && let EntryLocation::Fs(path) = &entry.location
                                    && app.dir_size_pending.contains(path)
                                {
                                    size_text = "…".to_string();
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
                                if rename_active {
                                    ui.scope_builder(
                                        egui::UiBuilder::new().max_rect(name_rect),
                                        |ui| {
                                            ui.set_clip_rect(name_rect);
                                            let rename = app
                                                .panel_mut(panel_side_for_closure)
                                                .browser
                                                .inline_rename
                                                .as_mut()
                                                .expect("rename active");
                                            let response = ui.add_sized(
                                                name_rect.size(),
                                                egui::TextEdit::singleline(&mut rename.text)
                                                    .font(egui::TextStyle::Body)
                                                    .id_source(match panel_side_for_closure {
                                                        ActivePanel::Left => "inline_rename_left",
                                                        ActivePanel::Right => "inline_rename_right",
                                                    }),
                                            );
                                            if rename.focus {
                                                response.request_focus();
                                                rename.focus = false;
                                            }
                                        },
                                    );
                                } else {
                                    ui.painter().with_clip_rect(name_rect).text(
                                        name_min,
                                        egui::Align2::LEFT_CENTER,
                                        entry.name.as_str(),
                                        font_id,
                                        color32(fg),
                                    );
                                }

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
                                }
                            });
                    },
                );
            });
        });

    if panel_response.response.contains_pointer() && ui.input(|i| i.pointer.any_pressed()) {
        app.active_panel = panel_side;
    }

    if let Some(top) = new_top_index {
        app.panel_mut(panel_side).browser.top_index = top;
    }

    if let Some(idx) = clicked_index {
        app.active_panel = panel_side;
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
    proxy: EventLoopProxy<UserEvent>,
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            runtime: None,
            proxy,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum UserEvent {
    Wake,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.runtime.is_some() {
            return;
        }

        let window = event_loop
            .create_window(
                WindowAttributes::default()
                    .with_title("Fileman (egui)")
                    .with_window_icon(app_icon()),
            )
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
        let (image_res_tx, image_res_rx) = mpsc::channel::<ImageResponse>();
        let (highlight_req_tx, highlight_req_rx) = mpsc::channel::<HighlightRequest>();
        let (highlight_res_tx, highlight_res_rx) = mpsc::channel::<HighlightResult>();
        let (edit_tx, edit_rx) = mpsc::channel::<EditLoadRequest>();
        let (edit_res_tx, edit_res_rx) = mpsc::channel::<EditLoadResult>();

        let proxy = self.proxy.clone();
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
                if let Some((image, meta)) = image {
                    let result = ImageResult {
                        key: req.key,
                        image,
                        meta,
                    };
                    let _ = image_res_tx.send(ImageResponse::Ok(result));
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
                    inline_rename: None,
                    sort_mode: SortMode::Name,
                    sort_desc: false,
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
                    inline_rename: None,
                    sort_mode: SortMode::Name,
                    sort_desc: false,
                },
                mode: PanelMode::Browser,
            },
            active_panel: ActivePanel::Left,
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
            props_dialog: None,
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
            meta: HashMap::new(),
            failures: HashMap::new(),
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

                let raw_input = runtime.egui_state.take_egui_input(&runtime.window);
                let output = runtime.egui_ctx.run(raw_input, |ctx| {
                    apply_theme(ctx, &runtime.app.theme.colors());
                    let input = ctx.input(|i| i.clone());
                    handle_keyboard(ctx, &input, &mut runtime.app, &mut runtime.ui_cache);
                    runtime.ui_cache.update_scroll_mode(&runtime.app);

                    for decoded in decoded_images.drain(..) {
                        match decoded {
                            ImageResponse::Ok(decoded) => {
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
                                runtime
                                    .image_cache
                                    .meta
                                    .insert(decoded.key.clone(), decoded.meta);
                                runtime.image_cache.pending.remove(&decoded.key);
                                runtime.image_cache.failures.remove(&decoded.key);
                                while runtime.image_cache.order.len() > MAX_IMAGE_TEXTURES {
                                    if let Some(old) = runtime.image_cache.order.pop_front()
                                        && old != decoded.key
                                    {
                                        runtime.image_cache.textures.remove(&old);
                                        runtime.image_cache.meta.remove(&old);
                                        runtime.image_cache.failures.remove(&old);
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

                    draw_command_bar(ctx, &runtime.app, &runtime.app.theme.colors());

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
                                if let PanelMode::Edit(ref mut edit) = panel.mode {
                                    draw_editor(
                                        ui,
                                        EditorRender {
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
                            } else if should_show_preview(&runtime.app, ActivePanel::Left) {
                                let is_focused = runtime.app.active_panel == ActivePanel::Left;
                                let theme = runtime.app.theme.clone();
                                let panel = runtime.app.panel_mut(ActivePanel::Left);
                                if let PanelMode::Preview(ref mut preview) = panel.mode {
                                    draw_preview(
                                        ui,
                                        PreviewRender {
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
                                if let PanelMode::Edit(ref mut edit) = panel.mode {
                                    draw_editor(
                                        ui,
                                        EditorRender {
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
                            } else if should_show_preview(&runtime.app, ActivePanel::Right) {
                                let is_focused = runtime.app.active_panel == ActivePanel::Right;
                                let theme = runtime.app.theme.clone();
                                let panel = runtime.app.panel_mut(ActivePanel::Right);
                                if let PanelMode::Preview(ref mut preview) = panel.mode {
                                    draw_preview(
                                        ui,
                                        PreviewRender {
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
                    if let Some(edit) = runtime.app.edit_panel_mut()
                        && edit.confirm_discard
                    {
                        draw_discard_modal(ctx, &mut runtime.app);
                    }
                    if runtime.app.props_dialog.is_some() {
                        draw_props_modal(ctx, &mut runtime.app);
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
                if highlight_updated {
                    runtime.window.request_redraw();
                }
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

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: UserEvent) {
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.needs_redraw = true;
            runtime.window.request_redraw();
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
            if !runtime.highlight_results.is_empty() {
                runtime.window.request_redraw();
            }
            if let Some(preview) = runtime.app.preview_panel_mut()
                && let Some(PreviewContent::Image(path)) = preview.content.as_ref()
            {
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
                            ContainerKind::Tar => "tar",
                            ContainerKind::TarGz => "tar.gz",
                            ContainerKind::TarBz2 => "tar.bz2",
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

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.shutdown();
        }
    }
}

#[derive(Default)]
struct CliArgs {
    snapshot: Option<PathBuf>,
    replay: Option<PathBuf>,
}

fn parse_cli_args() -> Result<CliArgs> {
    let mut args = std::env::args().skip(1);
    let mut parsed = CliArgs::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
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
            _ => {}
        }
    }
    Ok(parsed)
}

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
        _ => None,
    }
}

fn apply_replay_key(
    headless: &mut HeadlessUi,
    app: &mut AppState,
    ui_cache: &mut UiCache,
    key: &ReplayKey,
) {
    let modifiers = parse_modifiers(&key.modifiers);

    let mut events = Vec::new();
    let key_name = key.key.as_str();
    if key_name.eq_ignore_ascii_case("enter")
        && matches!(
            app.pending_op,
            Some(PendingOp::Delete { .. } | PendingOp::Copy { .. } | PendingOp::Move { .. })
        )
    {
        confirm_pending_op(app);
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
            ActivePanel::Left => ui_cache.left_rows.max(1),
            ActivePanel::Right => ui_cache.right_rows.max(1),
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

fn is_app_pending(app: &AppState) -> bool {
    let left = &app.left_panel.browser;
    let right = &app.right_panel.browser;
    let edit_loading = app.edit_panel().map(|edit| edit.loading).unwrap_or(false);
    let search_running = matches!(app.search_status, SearchStatus::Running(_));
    app.io_in_flight > 0
        || left.loading
        || right.loading
        || left.entries_rx.is_some()
        || right.entries_rx.is_some()
        || edit_loading
        || search_running
        || !app.dir_size_pending.is_empty()
}

fn drain_async(app: &mut AppState, max_iters: usize) {
    for _ in 0..max_iters {
        let changed = pump_async(app);
        let pending = is_app_pending(app);
        if !changed && !pending {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

fn wait_for_idle(
    headless: &mut HeadlessUi,
    app: &mut AppState,
    ui_cache: &mut UiCache,
    max_iters: usize,
) {
    for _ in 0..max_iters {
        let changed = pump_async(app);
        headless.run_frame(app, ui_cache, Vec::new());
        if !changed && !is_app_pending(app) && headless.highlight_pending.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

fn wait_for_duration(
    headless: &mut HeadlessUi,
    app: &mut AppState,
    ui_cache: &mut UiCache,
    duration: std::time::Duration,
) {
    let start = std::time::Instant::now();
    while start.elapsed() < duration {
        let _ = pump_async(app);
        headless.run_frame(app, ui_cache, Vec::new());
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

fn init_headless_app(root: Option<PathBuf>) -> Result<AppState> {
    let root = match root {
        Some(root) => root,
        None => std::env::current_dir().expect("current_dir"),
    };
    let (io_tx, io_rx, io_cancel_tx) = start_io_worker();
    let (preview_tx, preview_rx) = start_preview_worker();
    let (dir_size_tx, dir_size_rx) = start_dir_size_worker();
    let (search_tx, search_rx) = start_search_worker();
    let (edit_tx, edit_rx) = mpsc::channel::<EditLoadRequest>();
    let (edit_res_tx, edit_res_rx) = mpsc::channel::<EditLoadResult>();

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
                current_path: root.clone(),
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
                inline_rename: None,
                sort_mode: SortMode::Name,
                sort_desc: false,
            },
            mode: PanelMode::Browser,
        },
        right_panel: PanelState {
            browser: BrowserState {
                browser_mode: BrowserMode::Fs,
                current_path: root.clone(),
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
                inline_rename: None,
                sort_mode: SortMode::Name,
                sort_desc: false,
            },
            mode: PanelMode::Browser,
        },
        active_panel: ActivePanel::Left,
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
        props_dialog: None,
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
    Ok(app)
}

fn run_replay(case_path: &PathBuf, snapshot: Option<PathBuf>) -> Result<()> {
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
    load_fs_directory_async(&mut app, left_root, ActivePanel::Left, None);
    load_fs_directory_async(&mut app, right_root, ActivePanel::Right, None);

    let mut ui_cache = UiCache {
        left_rows: 20,
        right_rows: 20,
        scroll_mode: ScrollMode::Default,
        last_left_selected: 0,
        last_right_selected: 0,
        last_active_panel: ActivePanel::Left,
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
    run_replay_asserts(
        &repo_root,
        root.as_path(),
        &mut app,
        &mut ui_cache,
        &case.asserts,
    )?;
    Ok(())
}

struct HeadlessUi {
    egui_ctx: egui::Context,
    image_cache: ImageCache,
    highlight_cache: HashMap<String, egui::text::LayoutJob>,
    highlight_pending: HashSet<String>,
    image_req_tx: mpsc::Sender<ImageRequest>,
    highlight_req_tx: mpsc::Sender<HighlightRequest>,
    highlight_res_rx: mpsc::Receiver<HighlightResult>,
}

impl HeadlessUi {
    fn new() -> Self {
        let (image_req_tx, _image_req_rx) = mpsc::channel::<ImageRequest>();
        let (highlight_req_tx, highlight_req_rx) = mpsc::channel::<HighlightRequest>();
        let (highlight_res_tx, highlight_res_rx) = mpsc::channel::<HighlightResult>();
        thread::spawn(move || {
            while let Ok(req) = highlight_req_rx.recv() {
                let job = highlight_text_job(&req.text, req.ext.as_deref(), req.theme_kind);
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

    fn run_frame(&mut self, app: &mut AppState, ui_cache: &mut UiCache, events: Vec<egui::Event>) {
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
            handle_keyboard(ctx, &input, app, ui_cache);
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

struct UiRender<'a> {
    ctx: &'a egui::Context,
    app: &'a mut AppState,
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
    apply_theme(ctx, &app.theme.colors());
    draw_command_bar(ctx, app, &app.theme.colors());
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
                if should_show_editor(app, ActivePanel::Left) {
                    let is_focused = app.active_panel == ActivePanel::Left;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(ActivePanel::Left);
                    if let PanelMode::Edit(ref mut edit) = panel.mode {
                        draw_editor(
                            ui,
                            EditorRender {
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
                } else if should_show_preview(app, ActivePanel::Left) {
                    let is_focused = app.active_panel == ActivePanel::Left;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(ActivePanel::Left);
                    if let PanelMode::Preview(ref mut preview) = panel.mode {
                        draw_preview(
                            ui,
                            PreviewRender {
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
                } else {
                    draw_panel(
                        ui,
                        app,
                        ActivePanel::Left,
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
                if should_show_editor(app, ActivePanel::Right) {
                    let is_focused = app.active_panel == ActivePanel::Right;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(ActivePanel::Right);
                    if let PanelMode::Edit(ref mut edit) = panel.mode {
                        draw_editor(
                            ui,
                            EditorRender {
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
                } else if should_show_preview(app, ActivePanel::Right) {
                    let is_focused = app.active_panel == ActivePanel::Right;
                    let theme = app.theme.clone();
                    let panel = app.panel_mut(ActivePanel::Right);
                    if let PanelMode::Preview(ref mut preview) = panel.mode {
                        draw_preview(
                            ui,
                            PreviewRender {
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
                } else {
                    draw_panel(
                        ui,
                        app,
                        ActivePanel::Right,
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
        draw_confirmation(ctx, app);
    }
    if let Some(edit) = app.edit_panel_mut()
        && edit.confirm_discard
    {
        draw_discard_modal(ctx, app);
    }
    if app.props_dialog.is_some() {
        draw_props_modal(ctx, app);
    }
    if app.io_in_flight > 0 {
        draw_progress_modal(ctx, app);
    }
}

fn resolve_case_path(base: &std::path::Path, path: &PathBuf) -> PathBuf {
    if path.is_absolute() {
        path.clone()
    } else {
        base.join(path)
    }
}

fn collect_fs_entries(
    root: &std::path::Path,
    rel: &std::path::Path,
    out: &mut HashMap<String, FsEntryKind>,
) -> Result<()> {
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

fn assert_fs(root: &std::path::Path, fs: &FsAssert) -> Result<()> {
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

fn assert_files(root: &std::path::Path, files: &[FileAssert]) -> Result<()> {
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
    base: &std::path::Path,
    app: &mut AppState,
    ui_cache: &mut UiCache,
    snapshots: &[SnapshotAssert],
) -> Result<()> {
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
        let diff = compare_snapshots(
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

fn run_replay_asserts(
    base: &std::path::Path,
    root: &std::path::Path,
    app: &mut AppState,
    ui_cache: &mut UiCache,
    asserts: &ReplayAsserts,
) -> Result<()> {
    if let Some(fs) = asserts.fs.as_ref() {
        assert_fs(root, fs)?;
    }
    if !asserts.files.is_empty() {
        assert_files(root, &asserts.files)?;
    }
    if !asserts.snapshots.is_empty() {
        assert_snapshots(base, app, ui_cache, &asserts.snapshots)?;
    }
    Ok(())
}

fn render_snapshot(app: &mut AppState, ui_cache: &mut UiCache, path: &PathBuf) -> Result<()> {
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
        meta: HashMap::new(),
        failures: HashMap::new(),
        pending: HashSet::new(),
        order: VecDeque::new(),
    };
    let highlight_cache = build_snapshot_highlights(app);
    let mut highlight_pending = HashSet::new();

    app.preview_tx = preview_tx;
    app.preview_rx = preview_rx;
    app.io_tx = io_tx;
    app.io_rx = io_rx;
    app.io_cancel_tx = io_cancel_tx;
    app.dir_size_tx = dir_size_tx;
    app.dir_size_rx = dir_size_rx;
    app.edit_tx = edit_tx;
    app.edit_rx = edit_res_rx;
    app.search_tx = search_tx;
    app.search_rx = search_rx;

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
        draw_root_ui(UiRender {
            ctx,
            app,
            ui_cache,
            image_cache: &mut image_cache,
            image_req_tx: &image_req_tx,
            highlight_cache: &highlight_cache,
            highlight_pending: &mut highlight_pending,
            highlight_req_tx: &highlight_req_tx,
        });
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
    )
    .map_err(|err| anyhow::anyhow!(err))?;

    context.destroy_texture_view(view);
    context.destroy_texture(texture);
    context.destroy_buffer(result_buffer);
    painter.destroy(&context);
    context.destroy_command_encoder(&mut command_encoder);

    Ok(())
}

fn build_snapshot_highlights(app: &AppState) -> HashMap<String, egui::text::LayoutJob> {
    let mut cache = HashMap::new();
    let theme_kind = app.theme.kind;

    if let Some(edit) = app.edit_panel()
        && let Some(path) = edit.path.as_ref()
    {
        let base_key = format!("edit:{}", path.to_string_lossy());
        let key = format!("{base_key}:{}", edit.highlight_hash);
        let job = highlight_text_job(&edit.text, edit.ext.as_deref(), theme_kind);
        cache.insert(key, job);
    }

    if let Some(preview) = app.preview_panel()
        && let Some(PreviewContent::Text(text)) = preview.content.as_ref()
    {
        let base_key = preview.key.clone().unwrap_or_else(|| "unknown".to_string());
        let key = format!("{base_key}:{:x}", hash_text(text));
        let job = highlight_text_job(text, preview.ext.as_deref(), theme_kind);
        cache.insert(key, job);
    }

    cache
}
fn run_snapshot(path: &PathBuf) -> Result<()> {
    let mut app = init_headless_app(None)?;
    let mut ui_cache = UiCache {
        left_rows: 10,
        right_rows: 10,
        scroll_mode: ScrollMode::Default,
        last_left_selected: 0,
        last_right_selected: 0,
        last_active_panel: ActivePanel::Left,
        last_left_dir_token: 0,
        last_right_dir_token: 0,
    };
    let cur_dir = std::env::current_dir()?;
    load_fs_directory_async(&mut app, cur_dir.clone(), ActivePanel::Left, None);
    load_fs_directory_async(&mut app, cur_dir, ActivePanel::Right, None);
    drain_async(&mut app, 50);
    render_snapshot(&mut app, &mut ui_cache, path)
}

fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_module("egui", log::LevelFilter::Warn)
        .filter_module("egui_winit", log::LevelFilter::Warn)
        .init();

    let args = parse_cli_args()?;
    if let Some(replay_path) = args.replay.as_ref() {
        return run_replay(replay_path, args.snapshot);
    }
    if let Some(snapshot_path) = args.snapshot {
        return run_snapshot(&snapshot_path);
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy);
    event_loop
        .run_app(&mut app)
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}
