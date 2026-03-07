use fileman::app_state;

pub fn draw_theme_picker(ctx: &egui::Context, app: &mut app_state::AppState) {
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
