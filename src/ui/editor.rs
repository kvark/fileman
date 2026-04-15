use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use egui;

use fileman::{app_state, theme};

use crate::{HighlightRequest, color32, cursor_row_col, hash_text};

pub struct EditorRender<'a> {
    pub theme: &'a theme::Theme,
    pub is_focused: bool,
    pub edit: &'a mut app_state::EditState,
    pub highlight_cache: &'a HashMap<String, egui::text::LayoutJob>,
    pub highlight_pending: &'a mut HashSet<String>,
    pub highlight_req_tx: &'a mpsc::Sender<HighlightRequest>,
    pub min_height: f32,
}

pub fn draw_editor(ui: &mut egui::Ui, ctx: EditorRender<'_>) {
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
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut edit.wrap, "Wrap");
            });
            ui.add_space(4.0);
            if edit.loading {
                let t = ui.ctx().input(|i| i.time);
                let dots = ".".repeat(((t * 2.0) as usize % 4) + 1);
                ui.colored_label(color32(colors.row_fg_inactive), format!("Loading{dots}"));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(300));
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
            let editor_wrap = edit.wrap;
            let mut layouter = |ui: &egui::Ui, string: &dyn egui::TextBuffer, wrap_width: f32| {
                let effective_wrap = if editor_wrap {
                    wrap_width
                } else {
                    f32::INFINITY
                };
                if let Some(key) = key_with_hash.as_ref() {
                    let _wrap_changed = (edit.highlight_wrap_width - wrap_width).abs() > 0.5;
                    if let Some(cached) = highlight_cache.get(key) {
                        let mut job = cached.clone();
                        job.wrap.max_width = effective_wrap;
                        job.wrap.break_anywhere = editor_wrap;
                        return ui.fonts_mut(|f| f.layout_job(job));
                    }
                    needs_highlight = true;
                }
                let mut job = egui::text::LayoutJob::simple(
                    string.as_str().to_string(),
                    egui::TextStyle::Monospace.resolve(ui.style()),
                    egui::Color32::LIGHT_GRAY,
                    effective_wrap,
                );
                job.wrap.break_anywhere = editor_wrap;
                ui.fonts_mut(|f| f.layout_job(job))
            };
            let footer_height = ui.text_style_height(&egui::TextStyle::Body).max(1.0) + 8.0;
            let editor_height = (ui.available_height() - footer_height).max(0.0);
            let row_height = ui.text_style_height(&egui::TextStyle::Monospace).max(1.0);
            let desired_rows = (editor_height / row_height).floor().max(1.0) as usize;
            let mut edit_output: Option<egui::text_edit::TextEditOutput> = None;
            let scroll = if editor_wrap {
                egui::ScrollArea::vertical()
            } else {
                egui::ScrollArea::both()
            };
            scroll
                .id_salt("editor_scroll")
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .max_height(editor_height)
                .show(ui, |ui| {
                    let mut te = egui::TextEdit::multiline(&mut text)
                        .font(egui::TextStyle::Monospace)
                        .layouter(&mut layouter)
                        .cursor_at_end(false)
                        .id_source("editor_text")
                        .desired_rows(desired_rows)
                        .lock_focus(true);
                    if editor_wrap {
                        te = te.desired_width(f32::INFINITY);
                    } else {
                        te = te.code_editor();
                    }
                    let output = te.show(ui);
                    if editor_wrap {
                        paint_newline_markers(ui, &output);
                    }
                    edit_output = Some(output);
                });
            let response = edit_output
                .as_ref()
                .map(|output| output.response.clone())
                .unwrap_or_else(|| egui::AtomLayoutResponse::empty(ui.label(" ")));
            // Auto-indent: if Enter was just pressed, copy leading whitespace
            // from the previous line and insert it after the newline.
            // Auto-indent: if Enter was just pressed, copy leading whitespace
            // from the previous line. CCursor.index is a character offset.
            let cursor_char = edit_output
                .as_ref()
                .and_then(|o| o.cursor_range)
                .map(|r| r.primary.index);
            if let Some(char_pos) = cursor_char
                && char_pos > 0
            {
                // Convert character offset to byte offset
                let byte_pos: usize = text
                    .char_indices()
                    .nth(char_pos)
                    .map(|(i, _)| i)
                    .unwrap_or(text.len());
                if byte_pos > 0 && text.as_bytes().get(byte_pos - 1) == Some(&b'\n') {
                    let before = &text[..byte_pos - 1];
                    let prev_line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
                    let prev_line = &before[prev_line_start..];
                    let indent: String = prev_line
                        .chars()
                        .take_while(|c| *c == ' ' || *c == '\t')
                        .collect();
                    if !indent.is_empty() {
                        let indent_chars = indent.chars().count();
                        text.insert_str(byte_pos, &indent);
                        if let Some(ref mut output) = edit_output
                            && let Some(ref mut range) = output.cursor_range
                        {
                            range.primary.index += indent_chars;
                            range.secondary.index += indent_chars;
                        }
                    }
                }
            }
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
                    .request_repaint_after(std::time::Duration::from_millis(500));
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

fn paint_newline_markers(ui: &egui::Ui, output: &egui::text_edit::TextEditOutput) {
    let galley = &output.galley;
    let galley_pos = output.galley_pos;
    let color = egui::Color32::from_white_alpha(40);
    let font = egui::FontId::monospace(10.0);
    for placed_row in &galley.rows {
        if placed_row.ends_with_newline {
            let row_rect = placed_row.rect();
            let last_x = if let Some(last_glyph) = placed_row.row.glyphs.last() {
                last_glyph.pos.x + last_glyph.size().x
            } else {
                row_rect.left()
            };
            let pos = galley_pos + egui::vec2(last_x + 2.0, row_rect.center().y);
            ui.painter()
                .text(pos, egui::Align2::LEFT_CENTER, "↵", font.clone(), color);
        }
    }
}
