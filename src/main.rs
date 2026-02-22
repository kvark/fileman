use anyhow::Result;
use blade_egui::{GuiPainter, ScreenDescriptor};
use blade_graphics::{
    CommandEncoderDesc, Context, ContextDesc, Extent, FinishOp, InitOp, RenderTarget,
    RenderTargetSet, SurfaceConfig, TextureColor, TextureSubresources, TextureUsage,
    TextureViewDesc, ViewDimension,
};
use egui_winit::State as EguiWinitState;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::PathBuf,
    sync::mpsc,
    thread,
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowAttributes, WindowId},
};

use fileman::app_state::{AppState, PanelState};
use fileman::core::{
    ActivePanel, DirBatch, DirEntry, EntryLocation, PanelMode, PreviewContent, is_zip_path,
    read_zip_directory,
};
use fileman::theme::{Color, Theme, ThemeColors};
use fileman::workers::{start_io_worker, start_preview_worker};

const ROW_HEIGHT: f32 = 22.0;

struct UiCache {
    left_rows: usize,
    right_rows: usize,
}

struct ImageRequest {
    path: PathBuf,
}

struct ImageResult {
    path: PathBuf,
    image: egui::ColorImage,
}

struct ImageCache {
    textures: HashMap<PathBuf, egui::TextureHandle>,
    pending: HashSet<PathBuf>,
    order: VecDeque<PathBuf>,
}

const MAX_IMAGE_TEXTURES: usize = 64;
const MAX_IMAGE_UPLOADS_PER_FRAME: usize = 2;

fn touch_image(cache: &mut ImageCache, key: &PathBuf) {
    if let Some(pos) = cache.order.iter().position(|p| p == key) {
        cache.order.remove(pos);
        cache.order.push_back(key.clone());
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

fn apply_theme(ctx: &egui::Context, colors: &ThemeColors) {
    let mut style = (*ctx.style()).clone();
    style.visuals.window_fill = color32(colors.preview_bg);
    style.visuals.panel_fill = color32(colors.preview_bg);
    style.visuals.extreme_bg_color = color32(colors.header_bg);
    style.visuals.window_stroke.color = color32(colors.panel_border_inactive);
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

fn surface_error_help() -> &'static str {
    "Blade-graphics could not find a supported GPU backend.\n\
Try one of:\n\
  - Install Vulkan drivers for your GPU and re-run.\n\
  - Build with GLES fallback: RUSTFLAGS=\"--cfg gles\" cargo run\n\
On Linux in CI or headless environments, Vulkan is often unavailable."
}

fn panel_path_display(panel: &PanelState) -> String {
    match &panel.mode {
        PanelMode::Fs => panel.current_path.to_string_lossy().into_owned(),
        PanelMode::Zip { archive_path, cwd } => {
            if cwd.is_empty() {
                format!("{}::zip:/", archive_path.to_string_lossy())
            } else {
                format!("{}::zip:/{}", archive_path.to_string_lossy(), cwd)
            }
        }
    }
}

fn apply_dir_batch(panel: &mut PanelState, batch: DirBatch) {
    let prior_selection = panel
        .entries
        .get(panel.selected_index)
        .map(|e| e.name.clone());

    match batch {
        DirBatch::Append(mut new_entries) => {
            panel.entries.append(&mut new_entries);
        }
        DirBatch::Replace(new_entries) => {
            panel.entries = new_entries;
            panel.selected_index = 0;
            panel.top_index = 0;
        }
    }

    let restore_name = panel.prefer_select_name.take().or(prior_selection);
    if let Some(pref) = restore_name
        && let Some(idx) = panel.entries.iter().position(|e| e.name == pref)
    {
        panel.selected_index = idx;
    }
}

fn pump_async(app: &mut AppState) {
    for side in [ActivePanel::Left, ActivePanel::Right] {
        let panel = app.panel_mut(side.clone());
        if let Some(rx) = panel.entries_rx.take() {
            let mut handled = 0usize;
            while handled < 8 {
                match rx.try_recv() {
                    Ok(batch) => {
                        apply_dir_batch(panel, batch);
                        handled += 1;
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        panel.entries_rx = Some(rx);
                        break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
            }
        }
    }

    match app.preview_rx.try_recv() {
        Ok((id, content)) => {
            if id == app.preview_request_id {
                app.preview = Some(content);
            }
        }
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => {}
    }
}

fn load_fs_directory_async(
    app: &mut AppState,
    path: PathBuf,
    target_panel: ActivePanel,
    prefer_name: Option<String>,
) {
    let mut initial: Vec<DirEntry> = Vec::new();
    if path.parent().is_some() {
        initial.push(DirEntry {
            name: "..".to_string(),
            is_dir: true,
            location: EntryLocation::Fs(path.parent().unwrap().to_path_buf()),
        });
    }

    let (tx, rx) = mpsc::channel::<DirBatch>();
    let path_clone = path.clone();

    if let Ok(mut rd) = fs::read_dir(&path) {
        let mut snapshot: Vec<DirEntry> = Vec::with_capacity(128);
        for _ in 0..128 {
            match rd.next() {
                Some(Ok(entry)) => {
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                    snapshot.push(DirEntry {
                        name: file_name,
                        is_dir,
                        location: EntryLocation::Fs(entry.path()),
                    });
                }
                Some(Err(_)) | None => break,
            }
        }
        if !snapshot.is_empty() {
            let _ = tx.send(DirBatch::Append(snapshot));
        }
        thread::spawn(move || {
            let chunk = 500usize;
            let mut all: Vec<DirEntry> = Vec::new();
            for entry in rd.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();
                if let Ok(file_type) = entry.file_type() {
                    let is_dir = file_type.is_dir();
                    all.push(DirEntry {
                        name: file_name,
                        is_dir,
                        location: EntryLocation::Fs(entry.path()),
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
                        all.push(DirEntry {
                            name: file_name,
                            is_dir,
                            location: EntryLocation::Fs(entry.path()),
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
    panel_state.current_path = path.clone();
    panel_state.mode = PanelMode::Fs;
    panel_state.entries = initial;
    panel_state.selected_index = 0;
    panel_state.top_index = 0;
    panel_state.entries_rx = Some(rx);
    panel_state.prefer_select_name = remembered;
}

fn load_zip_directory_async(
    app: &mut AppState,
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
            location: EntryLocation::Zip {
                archive_path: archive_path.clone(),
                inner_path: parent,
            },
        });
    } else if let Some(parent) = archive_path.parent() {
        initial.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            location: EntryLocation::Fs(parent.to_path_buf()),
        });
    }

    let (tx, rx) = mpsc::channel::<DirBatch>();
    let ap = archive_path.clone();
    let cwd_clone = cwd.clone();

    if let Ok(mut all) = read_zip_directory(&ap, &cwd_clone) {
        if !all.is_empty() && all[0].name == ".." {
            all.remove(0);
        }
        let initial = all.iter().take(128).cloned().collect::<Vec<_>>();
        if !initial.is_empty() {
            let _ = tx.send(DirBatch::Append(initial));
        }
        thread::spawn(move || {
            let chunk = 500usize;
            let mut start = 128.min(all.len());
            while start < all.len() {
                let end = (start + chunk).min(all.len());
                let _ = tx.send(DirBatch::Append(all[start..end].to_vec()));
                start = end;
            }
        });
    }

    let remembered = prefer_name.clone().or_else(|| {
        app.zip_last_selected_name
            .get(&(archive_path.clone(), cwd.clone()))
            .cloned()
    });
    let panel_state = app.panel_mut(target_panel);

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
}

fn open_selected(app: &mut AppState) {
    let active = app.active_panel.clone();

    let (selected_entry, current_path, zip_cwd) = {
        let panel = app.get_active_panel();
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

    app.store_current_selection_memory();

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
                load_fs_directory_async(app, path.clone(), active.clone(), prefer_name);

                if selected_entry.name != ".."
                    && let Some(name) = app.fs_last_selected_name.get(path).cloned()
                {
                    app.select_entry_by_name(active, &name);
                }
            } else if is_zip_path(path) {
                load_zip_directory_async(app, path.clone(), "".to_string(), active, None);
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
                load_zip_directory_async(
                    app,
                    archive_path.clone(),
                    inner_path.clone(),
                    active.clone(),
                    prefer_name,
                );

                if selected_entry.name != ".."
                    && let Some(name) = app
                        .zip_last_selected_name
                        .get(&(archive_path.clone(), inner_path.clone()))
                        .cloned()
                {
                    app.select_entry_by_name(active, &name);
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

fn handle_keyboard(input: &egui::InputState, app: &mut AppState, cache: &UiCache) {
    let window_rows = active_window_rows(app, cache);
    if input.key_pressed(egui::Key::Tab) {
        app.switch_panel();
        if app.preview.is_some() {
            app.update_preview_for_current_selection();
        }
    }
    if input.key_pressed(egui::Key::Enter) {
        if app.theme_picker_open {
            app.apply_selected_theme();
        } else {
            open_selected(app);
        }
    }
    if input.key_pressed(egui::Key::ArrowDown) {
        if app.theme_picker_open {
            app.select_next_theme();
        } else {
            let panel = app.get_active_panel();
            if panel.selected_index + 1 < panel.entries.len() {
                app.select_entry(panel.selected_index + 1, window_rows);
            }
        }
    }
    if input.key_pressed(egui::Key::ArrowUp) {
        if app.theme_picker_open {
            app.select_prev_theme();
        } else {
            let panel = app.get_active_panel();
            if panel.selected_index > 0 {
                app.select_entry(panel.selected_index - 1, window_rows);
            }
        }
    }
    if input.key_pressed(egui::Key::PageUp) {
        let panel = app.get_active_panel();
        let new_index = panel.selected_index.saturating_sub(window_rows);
        app.select_entry(new_index, window_rows);
    }
    if input.key_pressed(egui::Key::PageDown) {
        let panel = app.get_active_panel();
        let len = panel.entries.len();
        let mut new_index = panel.selected_index.saturating_add(window_rows);
        if len > 0 && new_index >= len {
            new_index = len - 1;
        }
        app.select_entry(new_index, window_rows);
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
        app.enqueue_copy_selected();
    }
    if input.key_pressed(egui::Key::F9) {
        app.switch_theme();
    }
    if input.key_pressed(egui::Key::F10) {
        app.open_theme_picker();
    }
}

fn draw_preview(
    ui: &mut egui::Ui,
    app: &AppState,
    image_cache: &mut ImageCache,
    image_req_tx: &mpsc::Sender<ImageRequest>,
) {
    let colors = app.theme.colors();
    let header_bg = color32(colors.preview_header_bg);
    let header_fg = color32(colors.preview_header_fg);
    let text_color = color32(colors.preview_text);

    egui::Frame::NONE
        .fill(color32(colors.preview_bg))
        .show(ui, |ui| {
            egui::Frame::NONE.fill(header_bg).show(ui, |ui| {
                ui.colored_label(header_fg, "Preview (F3/Esc to close)");
            });
            ui.add_space(4.0);

            egui::ScrollArea::vertical().show(ui, |ui| match app.preview.as_ref() {
                Some(PreviewContent::Text(text)) => {
                    ui.colored_label(text_color, text);
                }
                Some(PreviewContent::Image(path)) => {
                    let key = path.to_path_buf();
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
                    } else {
                        if image_cache.pending.insert(key.clone()) {
                            let _ = image_req_tx.send(ImageRequest { path: key.clone() });
                        }
                        ui.colored_label(
                            text_color,
                            format!("Loading image...\n{}", key.to_string_lossy()),
                        );
                    }
                }
                None => {}
            });
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

fn draw_panel(
    ui: &mut egui::Ui,
    app: &mut AppState,
    panel_side: ActivePanel,
    image_cache: &mut ImageCache,
    image_req_tx: &mpsc::Sender<ImageRequest>,
) -> usize {
    let colors = app.theme.colors();
    let is_active = app.active_panel == panel_side;

    let panel = app.panel(panel_side.clone());
    let entries_len = panel.entries.len();
    let selected_index = panel.selected_index;
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

    let mut rows = 10usize;
    let mut clicked_index: Option<usize> = None;
    let mut open_on_double_click = false;
    let mut new_top_index: Option<usize> = None;
    let panel_side_for_closure = panel_side.clone();

    egui::Frame::NONE
        .stroke(egui::Stroke::new(
            1.0,
            color32(if is_active {
                colors.panel_border_active
            } else {
                colors.panel_border_inactive
            }),
        ))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                egui::Frame::NONE
                    .fill(color32(colors.header_bg))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if is_active {
                                ui.colored_label(color32(colors.header_fg), "●");
                            }
                            ui.colored_label(color32(colors.header_fg), header_text);
                        });
                    });

                let preview_open = app.preview.is_some() && !is_active;
                if preview_open {
                    draw_preview(ui, app, image_cache, image_req_tx);
                    rows = window_rows_for(ui.available_height(), ui.spacing().item_spacing.y);
                    return;
                }

                let list_height = (ui.available_height() - 24.0).max(0.0);
                rows = window_rows_for(list_height, ui.spacing().item_spacing.y);
                let mut visible_range = 0..0;

                egui::ScrollArea::vertical()
                    .id_salt(match panel_side_for_closure {
                        ActivePanel::Left => "left_list",
                        ActivePanel::Right => "right_list",
                    })
                    .show_rows(ui, ROW_HEIGHT, entries_len, |ui, row_range| {
                        visible_range = row_range.clone();
                        for idx in row_range {
                            let entry = &panel.entries[idx];
                            let is_selected = selected_index == idx;
                            let bg = if is_selected {
                                if is_active {
                                    colors.row_bg_selected_active
                                } else {
                                    colors.row_bg_selected_inactive
                                }
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
                            let prefix = if entry.is_dir { "d " } else { "f " };
                            let label = format!("{prefix}{}", entry.name);
                            let mut text = egui::RichText::new(label).color(color32(fg));
                            if entry.is_dir {
                                text = text.strong();
                            }

                            let response = egui::Frame::NONE
                                .fill(color32(bg))
                                .show(ui, |ui| {
                                    ui.add_sized(
                                        [ui.available_width(), ROW_HEIGHT],
                                        egui::Label::new(text).sense(egui::Sense::click()),
                                    )
                                })
                                .inner;

                            if is_selected && is_active {
                                ui.scroll_to_rect(response.rect, Some(egui::Align::Center));
                            }
                            if response.clicked() {
                                clicked_index = Some(idx);
                            }
                            if response.double_clicked() {
                                clicked_index = Some(idx);
                                open_on_double_click = true;
                            }
                        }
                    });

                if entries_len > 0 {
                    new_top_index = Some(visible_range.start.min(entries_len - 1));
                } else {
                    new_top_index = Some(0);
                }

                let selected_label = panel
                    .entries
                    .get(selected_index)
                    .map(|e| e.name.as_str())
                    .unwrap_or("-");
                let footer_text = format!("items: {entries_len} | selected: {selected_label}");

                egui::Frame::NONE
                    .fill(color32(colors.footer_bg))
                    .show(ui, |ui| {
                        ui.colored_label(color32(colors.footer_fg), footer_text);
                    });
            });
        });

    if let Some(top) = new_top_index {
        app.panel_mut(panel_side.clone()).top_index = top;
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
    image_req_tx: mpsc::Sender<ImageRequest>,
    image_res_rx: mpsc::Receiver<ImageResult>,
}

impl Runtime {
    fn shutdown(&mut self) {
        self.image_cache.textures.clear();
        self.image_cache.order.clear();
        self.image_cache.pending.clear();
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
        let io_tx = start_io_worker();
        let (preview_tx, preview_rx) = start_preview_worker();
        let (image_req_tx, image_req_rx) = mpsc::channel::<ImageRequest>();
        let (image_res_tx, image_res_rx) = mpsc::channel::<ImageResult>();

        thread::spawn(move || {
            while let Ok(req) = image_req_rx.recv() {
                if let Ok(img) = image::open(&req.path) {
                    let rgba = img.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    let result = ImageResult {
                        path: req.path,
                        image: egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
                    };
                    let _ = image_res_tx.send(result);
                }
            }
        });

        let mut app = AppState {
            left_panel: PanelState {
                current_path: cur_dir.clone(),
                mode: PanelMode::Fs,
                selected_index: 0,
                entries: Vec::new(),
                entries_rx: None,
                prefer_select_name: None,
                top_index: 0,
            },
            right_panel: PanelState {
                current_path: cur_dir.clone(),
                mode: PanelMode::Fs,
                selected_index: 0,
                entries: Vec::new(),
                entries_rx: None,
                prefer_select_name: None,
                top_index: 0,
            },
            active_panel: ActivePanel::Left,
            preview: None,
            preview_tx: preview_tx.clone(),
            preview_rx,
            preview_request_id: 0,
            io_tx,
            fs_last_selected_name: Default::default(),
            zip_last_selected_name: Default::default(),
            theme: Theme::dark(),
            theme_picker_open: false,
            theme_picker_selected: None,
        };

        app.theme
            .load_external_from_dir(std::path::Path::new("./themes"));
        load_fs_directory_async(&mut app, cur_dir.clone(), ActivePanel::Left, None);
        load_fs_directory_async(&mut app, cur_dir, ActivePanel::Right, None);

        let ui_cache = UiCache {
            left_rows: 10,
            right_rows: 10,
        };
        let image_cache = ImageCache {
            textures: HashMap::new(),
            pending: HashSet::new(),
            order: VecDeque::new(),
        };

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
            image_req_tx,
            image_res_rx,
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

        if runtime
            .egui_state
            .on_window_event(&runtime.window, &event)
            .consumed
        {
            return;
        }

        match event {
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
            }
            WindowEvent::RedrawRequested => {
                pump_async(&mut runtime.app);
                let mut decoded_images = Vec::new();
                while decoded_images.len() < MAX_IMAGE_UPLOADS_PER_FRAME {
                    match runtime.image_res_rx.try_recv() {
                        Ok(img) => decoded_images.push(img),
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    }
                }

                let raw_input = runtime.egui_state.take_egui_input(&runtime.window);
                let output = runtime.egui_ctx.run(raw_input, |ctx| {
                    apply_theme(ctx, &runtime.app.theme.colors());
                    let input = ctx.input(|i| i.clone());
                    handle_keyboard(&input, &mut runtime.app, &runtime.ui_cache);

                    for decoded in decoded_images.drain(..) {
                        let handle = ctx.load_texture(
                            format!("preview:{}", decoded.path.to_string_lossy()),
                            decoded.image,
                            egui::TextureOptions::LINEAR,
                        );
                        if !runtime.image_cache.textures.contains_key(&decoded.path) {
                            runtime.image_cache.order.push_back(decoded.path.clone());
                        }
                        runtime
                            .image_cache
                            .textures
                            .insert(decoded.path.clone(), handle);
                        runtime.image_cache.pending.remove(&decoded.path);
                        while runtime.image_cache.order.len() > MAX_IMAGE_TEXTURES {
                            if let Some(old) = runtime.image_cache.order.pop_front()
                                && old != decoded.path
                            {
                                runtime.image_cache.textures.remove(&old);
                            }
                        }
                    }

                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            runtime.ui_cache.left_rows = draw_panel(
                                ui,
                                &mut runtime.app,
                                ActivePanel::Left,
                                &mut runtime.image_cache,
                                &runtime.image_req_tx,
                            );
                            ui.separator();
                            runtime.ui_cache.right_rows = draw_panel(
                                ui,
                                &mut runtime.app,
                                ActivePanel::Right,
                                &mut runtime.image_cache,
                                &runtime.image_req_tx,
                            );
                        });
                    });

                    if runtime.app.theme_picker_open {
                        draw_theme_picker(ctx, &mut runtime.app);
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

                runtime.command_encoder.start();
                runtime.painter.update_textures(
                    &mut runtime.command_encoder,
                    &output.textures_delta,
                    &runtime.context,
                );

                let frame = runtime.surface.acquire_frame();
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
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Poll);
        if let Some(runtime) = self.runtime.as_ref() {
            runtime.window.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.shutdown();
        }
    }
}

fn main() -> Result<()> {
    env_logger::init();

    let event_loop = EventLoop::new()?;
    let mut app = App::new();
    event_loop
        .run_app(&mut app)
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}
