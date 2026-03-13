use fileman::theme;

use crate::color32;

pub fn draw_help(ui: &mut egui::Ui, theme: &theme::Theme, is_focused: bool, min_height: f32) {
    let colors = theme.colors();
    let version = env!("CARGO_PKG_VERSION");
    let shortcuts = [
        ("Enter", "Open"),
        ("Shift+Enter", "Open with system default app"),
        ("Tab / Ctrl+I", "Switch panels"),
        ("Ctrl+U", "Swap panels"),
        ("Alt+Left / Alt+Right", "Back / forward"),
        ("Backspace / Ctrl+PgUp", "Parent folder"),
        ("Ctrl+PgDn", "Open selected"),
        ("Ctrl+Left / Ctrl+Right", "Open selected dir in other panel"),
        ("F3", "Preview"),
        ("F4", "Edit"),
        ("Shift+F4", "New file"),
        ("F7", "New directory"),
        ("Insert", "Mark / unmark"),
        ("Shift+F6", "Rename"),
        ("F5", "Copy"),
        ("F6", "Move"),
        ("F8", "Delete"),
        ("Space", "Compute folder size"),
        ("Alt+F7", "Search"),
        ("Alt+Enter", "Properties"),
        ("Ctrl+R", "Refresh"),
        ("F9", "Toggle theme"),
        ("F10", "Theme picker"),
        ("F1", "Help"),
    ];
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
                    ui.colored_label(color32(colors.preview_header_fg), "Help (Tab to return)");
                });
            ui.add_space(8.0);
            ui.colored_label(color32(colors.preview_text), format!("Fileman {version}"));
            ui.colored_label(color32(colors.row_fg_inactive), "Author: Dzmitry Malyshau");
            ui.add_space(10.0);
            ui.colored_label(color32(colors.preview_text), "Shortcuts");
            ui.add_space(6.0);
            for (keys, desc) in shortcuts {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.colored_label(
                        color32(colors.row_fg_selected),
                        egui::RichText::new(keys).monospace().strong(),
                    );
                    ui.colored_label(color32(colors.row_fg_inactive), desc);
                });
            }
        });
}
