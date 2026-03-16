use fileman::app_state::{AsyncStatus, SearchStatus};
use fileman::theme;

use crate::color32;

pub fn draw_help(
    ui: &mut egui::Ui,
    theme: &theme::Theme,
    is_focused: bool,
    min_height: f32,
    async_status: &AsyncStatus,
) {
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
        ("Alt+F5", "Pack (create archive)"),
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

            // Async workers status
            ui.add_space(10.0);
            ui.colored_label(color32(colors.preview_text), "Async Workers");
            ui.add_space(6.0);
            draw_async_status(ui, &colors, async_status);

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

fn draw_async_status(
    ui: &mut egui::Ui,
    colors: &theme::ThemeColors,
    status: &AsyncStatus,
) {
    // IO worker
    let io_label = if status.io_in_flight == 0 {
        "idle".to_string()
    } else if status.io_cancel_requested {
        format!("{} tasks (cancelling)", status.io_in_flight)
    } else {
        format!("{} tasks in flight", status.io_in_flight)
    };
    draw_worker_row(ui, colors, "IO", &io_label, status.io_in_flight > 0);

    // Dir size worker
    let dir_label = if status.dir_size_pending == 0 {
        "idle".to_string()
    } else {
        format!("{} pending", status.dir_size_pending)
    };
    draw_worker_row(ui, colors, "Dir size", &dir_label, status.dir_size_pending > 0);

    // Search worker
    let (search_label, search_active) = match status.search {
        SearchStatus::Idle => ("idle".to_string(), false),
        SearchStatus::Running(progress) => (
            format!("scanning ({} scanned, {} matched)", progress.scanned, progress.matched),
            true,
        ),
        SearchStatus::Done(progress) => (
            format!("done ({} scanned, {} matched)", progress.scanned, progress.matched),
            false,
        ),
    };
    draw_worker_row(ui, colors, "Search", &search_label, search_active);
}

fn draw_worker_row(
    ui: &mut egui::Ui,
    colors: &theme::ThemeColors,
    name: &str,
    status: &str,
    active: bool,
) {
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.colored_label(
            color32(colors.row_fg_selected),
            egui::RichText::new(format!("{name}:")).monospace().strong(),
        );
        let color = if active {
            colors.row_fg_active
        } else {
            colors.row_fg_inactive
        };
        ui.colored_label(color32(color), status);
    });
}
