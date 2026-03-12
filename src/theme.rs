use serde::Deserialize;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    Dark,
    Light,
}

#[derive(Clone, Copy)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Clone)]
pub struct Theme {
    pub kind: ThemeKind,
    pub external: Vec<(String, ThemeColors)>,
    pub selected_external: Option<usize>,
}

#[derive(Clone)]
pub struct ThemeColors {
    pub divider: Color,
    pub row_bg_stripe: Color,
    pub row_bg_selected_active: Color,
    pub row_bg_selected_inactive: Color,
    pub row_fg_selected: Color,
    pub row_fg_active: Color,
    pub row_fg_inactive: Color,
    pub panel_border_active: Color,
    pub panel_border_inactive: Color,
    pub header_bg: Color,
    pub header_fg: Color,
    pub footer_bg: Color,
    pub footer_fg: Color,
    pub preview_bg: Color,
    pub preview_header_bg: Color,
    pub preview_header_fg: Color,
    pub preview_text: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            kind: ThemeKind::Dark,
            external: Vec::new(),
            selected_external: None,
        }
    }

    pub fn light() -> Self {
        Self {
            kind: ThemeKind::Light,
            external: Vec::new(),
            selected_external: None,
        }
    }

    pub fn set_external(&mut self, themes: Vec<(String, ThemeColors)>) {
        self.external = themes;
        self.selected_external = if self.external.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    pub fn toggle(&mut self) {
        if !self.external.is_empty() {
            let next = match self.selected_external {
                None => 0,
                Some(i) => (i + 1) % self.external.len(),
            };
            self.selected_external = Some(next);
            return;
        }
        self.kind = match self.kind {
            ThemeKind::Dark => ThemeKind::Light,
            ThemeKind::Light => ThemeKind::Dark,
        };
    }

    pub fn colors(&self) -> ThemeColors {
        if let Some(i) = self.selected_external {
            return self.external[i].1.clone();
        }
        match self.kind {
            ThemeKind::Dark => ThemeColors {
                divider: Color::rgba(0.18, 0.2, 0.24, 1.0),
                row_bg_stripe: Color::rgba(1.0, 1.0, 1.0, 0.04),
                row_bg_selected_active: Color::rgba(0.16, 0.38, 0.78, 1.0),
                row_bg_selected_inactive: Color::rgba(0.12, 0.27, 0.52, 1.0),
                row_fg_selected: Color::rgba(1.0, 1.0, 1.0, 1.0),
                row_fg_active: Color::rgba(0.92, 0.94, 0.97, 1.0),
                row_fg_inactive: Color::rgba(0.68, 0.73, 0.8, 1.0),
                panel_border_active: Color::rgba(0.32, 0.63, 0.98, 1.0),
                panel_border_inactive: Color::rgba(0.18, 0.2, 0.26, 1.0),
                header_bg: Color::rgba(0.12, 0.36, 0.66, 1.0),
                header_fg: Color::rgba(0.95, 0.98, 1.0, 1.0),
                footer_bg: Color::rgba(0.08, 0.1, 0.13, 1.0),
                footer_fg: Color::rgba(0.74, 0.8, 0.88, 1.0),
                preview_bg: Color::rgba(0.06, 0.07, 0.09, 1.0),
                preview_header_bg: Color::rgba(0.11, 0.13, 0.18, 1.0),
                preview_header_fg: Color::rgba(0.9, 0.94, 1.0, 1.0),
                preview_text: Color::rgba(0.88, 0.91, 0.95, 1.0),
            },
            ThemeKind::Light => ThemeColors {
                divider: Color::rgba(0.82, 0.84, 0.88, 1.0),
                row_bg_stripe: Color::rgba(0.0, 0.0, 0.0, 0.04),
                row_bg_selected_active: Color::rgba(0.45, 0.68, 0.98, 1.0),
                row_bg_selected_inactive: Color::rgba(0.7, 0.82, 0.98, 1.0),
                row_fg_selected: Color::rgba(0.02, 0.04, 0.08, 1.0),
                row_fg_active: Color::rgba(0.12, 0.15, 0.2, 1.0),
                row_fg_inactive: Color::rgba(0.35, 0.38, 0.45, 1.0),
                panel_border_active: Color::rgba(0.24, 0.55, 0.92, 1.0),
                panel_border_inactive: Color::rgba(0.78, 0.8, 0.84, 1.0),
                header_bg: Color::rgba(0.2, 0.45, 0.78, 1.0),
                header_fg: Color::rgba(0.98, 0.99, 1.0, 1.0),
                footer_bg: Color::rgba(0.92, 0.94, 0.97, 1.0),
                footer_fg: Color::rgba(0.25, 0.3, 0.38, 1.0),
                preview_bg: Color::rgba(0.98, 0.98, 0.99, 1.0),
                preview_header_bg: Color::rgba(0.9, 0.92, 0.96, 1.0),
                preview_header_fg: Color::rgba(0.14, 0.18, 0.24, 1.0),
                preview_text: Color::rgba(0.12, 0.16, 0.22, 1.0),
            },
        }
    }

    pub fn load_external_from_dir(&mut self, dir: &std::path::Path) {
        let themes = load_themes_from_dir(dir);
        if !themes.is_empty() {
            self.set_external(themes);
        }
    }
}

#[derive(Deserialize, Clone, Default)]
struct SerializableColor {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

#[derive(Deserialize, Default)]
struct ThemeFileColors {
    divider: Option<SerializableColor>,
    row_bg_stripe: Option<SerializableColor>,
    row_bg_selected_active: Option<SerializableColor>,
    row_bg_selected_inactive: Option<SerializableColor>,
    row_fg_selected: Option<SerializableColor>,
    row_fg_active: Option<SerializableColor>,
    row_fg_inactive: Option<SerializableColor>,
    panel_border_active: Option<SerializableColor>,
    panel_border_inactive: Option<SerializableColor>,
    header_bg: Option<SerializableColor>,
    header_fg: Option<SerializableColor>,
    footer_bg: Option<SerializableColor>,
    footer_fg: Option<SerializableColor>,
    preview_bg: Option<SerializableColor>,
    preview_header_bg: Option<SerializableColor>,
    preview_header_fg: Option<SerializableColor>,
    preview_text: Option<SerializableColor>,
}

#[derive(Deserialize, Default)]
struct ThemeFile {
    name: Option<String>,
    colors: Option<ThemeFileColors>,
}

fn rgba_from(c: &SerializableColor) -> Color {
    Color::rgba(c.r, c.g, c.b, c.a)
}

fn merge_colors(base: &ThemeColors, patch: &ThemeFileColors) -> ThemeColors {
    ThemeColors {
        divider: patch
            .divider
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.divider),
        row_bg_stripe: patch
            .row_bg_stripe
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.row_bg_stripe),
        row_bg_selected_active: patch
            .row_bg_selected_active
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.row_bg_selected_active),
        row_bg_selected_inactive: patch
            .row_bg_selected_inactive
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.row_bg_selected_inactive),
        row_fg_selected: patch
            .row_fg_selected
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.row_fg_selected),
        row_fg_active: patch
            .row_fg_active
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.row_fg_active),
        row_fg_inactive: patch
            .row_fg_inactive
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.row_fg_inactive),
        panel_border_active: patch
            .panel_border_active
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.panel_border_active),
        panel_border_inactive: patch
            .panel_border_inactive
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.panel_border_inactive),
        header_bg: patch
            .header_bg
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.header_bg),
        header_fg: patch
            .header_fg
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.header_fg),
        footer_bg: patch
            .footer_bg
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.footer_bg),
        footer_fg: patch
            .footer_fg
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.footer_fg),
        preview_bg: patch
            .preview_bg
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.preview_bg),
        preview_header_bg: patch
            .preview_header_bg
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.preview_header_bg),
        preview_header_fg: patch
            .preview_header_fg
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.preview_header_fg),
        preview_text: patch
            .preview_text
            .as_ref()
            .map(rgba_from)
            .unwrap_or(base.preview_text),
    }
}

fn parse_theme_bytes(name_hint: &str, bytes: &[u8]) -> Option<(String, ThemeColors)> {
    let dark_base = Theme::dark().colors();
    if let Ok(tf) = serde_json::from_slice::<ThemeFile>(bytes) {
        let name = tf.name.unwrap_or_else(|| name_hint.to_string());
        let colors = tf
            .colors
            .map(|c| merge_colors(&dark_base, &c))
            .unwrap_or(dark_base.clone());
        return Some((name, colors));
    }
    if let Ok(tf) = serde_yaml::from_slice::<ThemeFile>(bytes) {
        let name = tf.name.unwrap_or_else(|| name_hint.to_string());
        let colors = tf
            .colors
            .map(|c| merge_colors(&dark_base, &c))
            .unwrap_or(dark_base.clone());
        return Some((name, colors));
    }
    if let Ok(s) = std::str::from_utf8(bytes)
        && let Ok(tf) = toml::from_str::<ThemeFile>(s)
    {
        let name = tf.name.unwrap_or_else(|| name_hint.to_string());
        let colors = tf
            .colors
            .map(|c| merge_colors(&dark_base, &c))
            .unwrap_or(dark_base.clone());
        return Some((name, colors));
    }
    None
}

fn load_themes_from_dir(dir: &std::path::Path) -> Vec<(String, ThemeColors)> {
    let mut out = Vec::new();
    let mut candidates = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name_hint = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("theme")
                .to_string();
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(ext.as_str(), "json" | "yaml" | "yml" | "toml") {
                continue;
            }
            candidates.push((name_hint, path));
        }
    }
    candidates.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));
    for (name_hint, path) in candidates {
        if let Ok(bytes) = std::fs::read(&path)
            && let Some((name, colors)) = parse_theme_bytes(&name_hint, &bytes)
        {
            out.push((name, colors));
        }
    }
    out
}
