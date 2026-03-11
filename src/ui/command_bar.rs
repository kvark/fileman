use fileman::{app_state, theme};

use crate::color32;

pub fn draw_command_bar(
    ctx: &egui::Context,
    app: &app_state::AppState,
    colors: &theme::ThemeColors,
) {
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
                        draw_refresh_indicator(ui, app, colors);
                        draw_key_cap(ui, "F1", "Help", colors);
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

fn draw_refresh_indicator(
    ui: &mut egui::Ui,
    app: &app_state::AppState,
    colors: &theme::ThemeColors,
) {
    let frames = ["|", "/", "-", "\\"];
    let symbol = frames[(app.refresh_tick as usize) % frames.len()];
    let text = egui::RichText::new(symbol).color(color32(colors.footer_fg));
    egui::Frame::NONE
        .fill(color32(colors.preview_header_bg))
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(text);
        });
    ui.add_space(10.0);
}

fn draw_key_cap(ui: &mut egui::Ui, key: &str, label: &str, colors: &theme::ThemeColors) {
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
