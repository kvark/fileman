use egui;
use users;

use fileman::{app_state, core};

use crate::{color32, refresh_active_panel};

pub fn draw_props_modal(ctx: &egui::Context, app: &mut app_state::AppState) {
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
    changed_color: egui::Color32,
    normal_color: egui::Color32,
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

fn apply_props_dialog(app: &mut app_state::AppState, recursive: bool) {
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
    if let Err(e) = app.io_tx.send(core::IOTask::SetProps {
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

fn parse_user_value(input: &str) -> Result<u32, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Owner user is required".to_string());
    }
    if let Ok(uid) = trimmed.parse::<u32>() {
        return Ok(uid);
    }
    users::get_user_by_name(trimmed)
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
    users::get_group_by_name(trimmed)
        .map(|group| group.gid())
        .ok_or_else(|| format!("Unknown group: {trimmed}"))
}
