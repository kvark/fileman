use fileman::app_state;
use fileman::settings::{Bookmark, Settings, ThemePref};
use fileman::theme;

use crate::color32;

/// Possible outcomes of one frame of the settings modal.
#[derive(PartialEq, Eq)]
pub enum SettingsOutcome {
    Stay,
    Save,
    Cancel,
}

/// Draw the settings modal. Mutates `draft` in place; reading
/// `app.theme.external` to populate the theme dropdown. Returns the user's
/// intent for this frame.
pub fn draw_settings(
    ctx: &egui::Context,
    theme_for_colors: &theme::Theme,
    external_themes: &[(String, theme::ThemeColors)],
    draft: &mut Settings,
) -> SettingsOutcome {
    let colors = theme_for_colors.colors();
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let mut outcome = SettingsOutcome::Stay;

    // Background overlay
    let screen = ctx.content_rect();
    let overlay_layer = egui::LayerId::new(egui::Order::Foreground, "settings_overlay".into());
    ctx.layer_painter(overlay_layer).rect_filled(
        screen,
        egui::CornerRadius::ZERO,
        egui::Color32::from_black_alpha(160),
    );

    egui::Window::new("Settings")
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // Theme picker
            section_header(ui, &colors, "Theme");
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(matches!(draft.theme, ThemePref::Dark), "Dark")
                    .clicked()
                {
                    draft.theme = ThemePref::Dark;
                }
                if ui
                    .selectable_label(matches!(draft.theme, ThemePref::Light), "Light")
                    .clicked()
                {
                    draft.theme = ThemePref::Light;
                }
                for (name, _) in external_themes {
                    let selected = matches!(draft.theme, ThemePref::External(ref n) if n == name);
                    if ui.selectable_label(selected, name).clicked() {
                        draft.theme = ThemePref::External(name.clone());
                    }
                }
            });

            ui.add_space(10.0);
            section_header(ui, &colors, "Appearance");
            ui.checkbox(&mut draft.show_glyphs, "Show file-type glyphs");
            ui.checkbox(&mut draft.row_striping, "Alternating row stripes");

            ui.add_space(10.0);
            section_header(ui, &colors, "Behavior");
            ui.checkbox(
                &mut draft.auto_refresh,
                "Auto-refresh SFTP directories on back-navigation",
            );

            ui.add_space(10.0);
            section_header(ui, &colors, "SFTP Bookmarks");
            bookmark_editor(ui, &colors, &mut draft.bookmarks);

            ui.add_space(14.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new("Save").min_size(egui::vec2(96.0, 0.0)))
                    .clicked()
                {
                    outcome = SettingsOutcome::Save;
                }
                if ui
                    .add(egui::Button::new("Cancel").min_size(egui::vec2(96.0, 0.0)))
                    .clicked()
                {
                    outcome = SettingsOutcome::Cancel;
                }
                if let Some(path) = fileman::settings::settings_path() {
                    ui.add_space(12.0);
                    ui.colored_label(
                        color32(colors.row_fg_inactive),
                        egui::RichText::new(format!("→ {}", path.display())).small(),
                    );
                }
            });
        });

    if escape && outcome == SettingsOutcome::Stay {
        outcome = SettingsOutcome::Cancel;
    }
    outcome
}

fn section_header(ui: &mut egui::Ui, colors: &theme::ThemeColors, text: &str) {
    ui.colored_label(
        color32(colors.preview_text),
        egui::RichText::new(text).strong(),
    );
    ui.add_space(4.0);
}

fn bookmark_editor(
    ui: &mut egui::Ui,
    colors: &theme::ThemeColors,
    bookmarks: &mut Vec<Bookmark>,
) {
    let mut to_remove: Option<usize> = None;
    for (i, bm) in bookmarks.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut bm.label)
                    .desired_width(120.0)
                    .hint_text("label"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut bm.host)
                    .desired_width(140.0)
                    .hint_text("host"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut bm.path)
                    .desired_width(180.0)
                    .hint_text("/path"),
            );
            if ui
                .small_button(egui::RichText::new("✕").monospace())
                .clicked()
            {
                to_remove = Some(i);
            }
        });
    }
    if let Some(i) = to_remove {
        bookmarks.remove(i);
    }
    if ui.small_button("+ Add bookmark").clicked() {
        bookmarks.push(Bookmark {
            label: String::new(),
            host: String::new(),
            path: "/".to_string(),
        });
    }
    if bookmarks.is_empty() {
        ui.colored_label(
            color32(colors.row_fg_inactive),
            egui::RichText::new("No bookmarks. Click + Add to create one.").small(),
        );
    }
}

/// Open the settings modal: populate the draft from current settings.
pub fn open(app: &mut app_state::AppState) {
    app.settings_draft = Some(app.settings.clone());
}

/// Close without saving: drop the draft.
pub fn cancel(app: &mut app_state::AppState) {
    app.settings_draft = None;
}

/// Apply the draft as the new settings, persist to disk, and close. The
/// theme preference is also applied to the live theme.
pub fn save(app: &mut app_state::AppState) {
    if let Some(draft) = app.settings_draft.take() {
        app.settings = draft;
        crate::apply_theme_preference(&mut app.theme, &app.settings.theme);
        if let Err(e) = fileman::settings::save(&app.settings) {
            app.record_error("settings", format!("save failed: {e}"));
        }
    }
}
