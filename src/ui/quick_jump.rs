use egui;

use fileman::app_state;

use crate::color32;

pub struct QuickJumpResult {
    pub path: std::path::PathBuf,
    pub category: app_state::QuickJumpCategory,
}

fn category_group(cat: app_state::QuickJumpCategory) -> u8 {
    match cat {
        app_state::QuickJumpCategory::Remote => 0,
        app_state::QuickJumpCategory::Home | app_state::QuickJumpCategory::Mount => 1,
        app_state::QuickJumpCategory::Ssh => 2,
    }
}

pub fn draw_quick_jump(
    ctx: &egui::Context,
    app: &mut app_state::AppState,
) -> Option<QuickJumpResult> {
    let qj = app.quick_jump.as_mut()?;

    let colors = app.theme.colors();
    let screen = ctx.content_rect();
    let overlay_layer = egui::LayerId::new(egui::Order::Foreground, "quick_jump_overlay".into());
    ctx.layer_painter(overlay_layer).rect_filled(
        screen,
        egui::CornerRadius::ZERO,
        egui::Color32::from_black_alpha(160),
    );

    let mut result: Option<QuickJumpResult> = None;
    let prev_input = qj.input.clone();

    egui::Window::new("Go To")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_width(400.0)
        .show(ctx, |ui| {
            let text_response = ui.add(
                egui::TextEdit::singleline(&mut qj.input)
                    .desired_width(380.0)
                    .hint_text("Path..."),
            );
            if qj.focus_input {
                text_response.request_focus();
                qj.focus_input = false;
            }

            // Enter on the text field: navigate to typed path or selected item
            if text_response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if !qj.input.is_empty() && qj.filtered.is_empty() {
                    let expanded = expand_tilde(&qj.input);
                    result = Some(QuickJumpResult {
                        path: std::path::PathBuf::from(expanded),
                        category: app_state::QuickJumpCategory::Home,
                    });
                } else if !qj.filtered.is_empty() {
                    let entry = &qj.entries[qj.filtered[qj.selected]];
                    result = Some(QuickJumpResult {
                        path: entry.path.clone(),
                        category: entry.category,
                    });
                }
            }

            ui.add_space(4.0);

            let row_height = 20.0;
            let separator_height = 5.0;
            let max_rows = 12;
            let visible = qj.filtered.len().min(max_rows);
            let scroll_height = (visible as f32) * row_height + 8.0;

            egui::ScrollArea::vertical()
                .max_height(scroll_height)
                .show(ui, |ui| {
                    let mut prev_group: Option<u8> = None;
                    for (fi, &idx) in qj.filtered.iter().enumerate() {
                        let entry = &qj.entries[idx];
                        let group = category_group(entry.category);

                        // Draw separator between groups
                        if let Some(pg) = prev_group {
                            if pg != group {
                                let (sep_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), separator_height),
                                    egui::Sense::hover(),
                                );
                                if ui.is_rect_visible(sep_rect) {
                                    let y = sep_rect.center().y;
                                    ui.painter().line_segment(
                                        [
                                            egui::pos2(sep_rect.left() + 4.0, y),
                                            egui::pos2(sep_rect.right() - 4.0, y),
                                        ],
                                        egui::Stroke::new(2.0, color32(colors.panel_border_active)),
                                    );
                                }
                            }
                        }
                        prev_group = Some(group);

                        let is_selected = fi == qj.selected;
                        let bg = if is_selected {
                            color32(colors.row_bg_selected_active)
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        let fg = if is_selected {
                            color32(colors.row_fg_selected)
                        } else {
                            color32(colors.row_fg_active)
                        };

                        let text = format!("  {}", entry.label);
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), row_height),
                            egui::Sense::click(),
                        );
                        if ui.is_rect_visible(rect) {
                            if is_selected {
                                ui.painter().rect_filled(rect, 0.0, bg);
                            }
                            ui.painter().text(
                                rect.left_center(),
                                egui::Align2::LEFT_CENTER,
                                &text,
                                egui::FontId::proportional(14.0),
                                fg,
                            );
                        }
                        if is_selected {
                            resp.scroll_to_me(Some(egui::Align::Center));
                        }
                        if resp.clicked() {
                            result = Some(QuickJumpResult {
                                path: entry.path.clone(),
                                category: entry.category,
                            });
                        }
                    }
                });
        });

    // Refilter if input changed
    if qj.input != prev_input {
        refilter(qj);
    }

    result
}

fn refilter(qj: &mut app_state::QuickJumpState) {
    let query = qj.input.to_lowercase();
    let tokens: Vec<&str> = query.split_whitespace().collect();
    qj.filtered = (0..qj.entries.len())
        .filter(|&i| {
            if tokens.is_empty() {
                return true;
            }
            let label = qj.entries[i].label.to_lowercase();
            let path = qj.entries[i].path.to_string_lossy().to_lowercase();
            tokens.iter().all(|t| label.contains(t) || path.contains(t))
        })
        .collect();
    qj.selected = 0;
}

fn expand_tilde(input: &str) -> String {
    if (input.starts_with("~/") || input == "~")
        && let Ok(home) = std::env::var("HOME")
    {
        return input.replacen('~', &home, 1);
    }
    input.to_string()
}
