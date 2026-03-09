use std::sync::mpsc;

use fileman::{app_state, core, theme};

use crate::input::open_selected;
use crate::{
    ImageCache, ImageRequest, ROW_HEIGHT, SIZE_COL_WIDTH, ScrollMode, blend_color, color32,
    fade_color, panel_path_display, reload_panel, resort_browser_entries, sort_mode_label,
    window_rows_for,
};

pub fn draw_panel(
    ui: &mut egui::Ui,
    app: &mut app_state::AppState,
    panel_side: core::ActivePanel,
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
        _loading_progress,
        top_index,
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
            browser.top_index,
        )
    };

    let mut rows = 10usize;
    let mut clicked_index: Option<usize> = None;
    let mut open_on_double_click = false;
    let mut new_top_index: Option<usize> = None;
    let panel_side_for_closure = panel_side;

    let mut request_raw_reload = false;
    let panel_response = egui::Frame::NONE
        .fill(color32(theme::Color::rgba(0.0, 0.0, 0.0, 0.0)))
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
                                    let mut header_display = if is_active {
                                        format!("● {header_text}")
                                    } else {
                                        header_text.clone()
                                    };
                                    if loading {
                                        header_display = format!("⟳ {header_display}");
                                    }
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
                                                core::ActivePanel::Left => "left_sort_mode",
                                                core::ActivePanel::Right => "right_sort_mode",
                                            })
                                            .selected_text(sort_mode_label(sort_mode))
                                            .show_ui(
                                                ui,
                                                |ui| {
                                                    sort_changed |= ui
                                                        .selectable_value(
                                                            &mut sort_mode,
                                                            core::SortMode::Name,
                                                            "Name",
                                                        )
                                                        .changed();
                                                    sort_changed |= ui
                                                        .selectable_value(
                                                            &mut sort_mode,
                                                            core::SortMode::Date,
                                                            "Date",
                                                        )
                                                        .changed();
                                                    sort_changed |= ui
                                                        .selectable_value(
                                                            &mut sort_mode,
                                                            core::SortMode::Size,
                                                            "Size",
                                                        )
                                                        .changed();
                                                    sort_changed |= ui
                                                        .selectable_value(
                                                            &mut sort_mode,
                                                            core::SortMode::Raw,
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
                                    if sort_mode == core::SortMode::Raw
                                        && previous_sort_mode != core::SortMode::Raw
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
                }

                let list_height = (ui.available_height() - footer_height - spacing).max(0.0);
                rows = window_rows_for(list_height, ui.spacing().item_spacing.y);
                let mut visible_range = 0..0;

                let mut scroll_target = None;
                if is_active && entries_len > 0 {
                    let row_height = ROW_HEIGHT + ui.spacing().item_spacing.y;
                    let total_height =
                        (row_height * entries_len as f32 - ui.spacing().item_spacing.y).max(0.0);
                    let ensure_visible = selected_index < top_index
                        || selected_index >= top_index.saturating_add(rows);
                    let center_offset = (list_height - row_height) * 0.5;
                    let mut target = if ensure_visible || scroll_mode == ScrollMode::ForceActive {
                        selected_index as f32 * row_height - center_offset
                    } else {
                        0.0
                    };
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
                    if ensure_visible || scroll_mode == ScrollMode::ForceActive {
                        scroll_target = Some(target);
                    }
                }

                ui.allocate_ui_with_layout(
                    egui::Vec2::new(ui.available_width(), list_height),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        let mut scroll = egui::ScrollArea::vertical()
                            .id_salt(match panel_side_for_closure {
                                core::ActivePanel::Left => "left_list",
                                core::ActivePanel::Right => "right_list",
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
                                    theme::Color::rgba(0.0, 0.0, 0.0, 0.06)
                                } else {
                                    theme::Color::rgba(0.0, 0.0, 0.0, 0.0)
                                };
                                let fg = if is_selected {
                                    colors.row_fg_selected
                                } else if is_active {
                                    colors.row_fg_active
                                } else {
                                    colors.row_fg_inactive
                                };
                                let mut fg = fg;
                                let is_hidden =
                                    entry.name.starts_with('.') && entry.name.as_str() != "..";
                                let file_tint = if entry.is_dir {
                                    None
                                } else if core::is_text_name(&entry.name) {
                                    Some(theme::Color::rgba(0.22, 0.78, 0.56, 1.0))
                                } else if core::is_media_name(&entry.name) {
                                    Some(theme::Color::rgba(0.32, 0.68, 1.0, 1.0))
                                } else {
                                    Some(theme::Color::rgba(0.92, 0.68, 0.28, 1.0))
                                };
                                if !is_selected && let Some(tint) = file_tint {
                                    let factor = if is_active { 0.42 } else { 0.32 };
                                    fg = blend_color(fg, tint, factor);
                                }
                                if is_hidden && !is_selected {
                                    fg = fade_color(fg, 0.55);
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
                                let icon_color = if is_hidden && !is_selected {
                                    fade_color(icon_color, 0.55)
                                } else {
                                    icon_color
                                };
                                ui.painter().rect_filled(
                                    icon_rect,
                                    egui::CornerRadius::same(2),
                                    color32(icon_color),
                                );

                                let font_id = egui::TextStyle::Body.resolve(ui.style());
                                let mut size_text =
                                    entry.size.map(core::format_size).unwrap_or_default();
                                if size_text.is_empty()
                                    && entry.is_dir
                                    && let core::EntryLocation::Fs(path) = &entry.location
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
                                                        core::ActivePanel::Left => {
                                                            "inline_rename_left"
                                                        }
                                                        core::ActivePanel::Right => {
                                                            "inline_rename_right"
                                                        }
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
                                if is_active && app.search_ui == app_state::SearchUiState::Open {
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
