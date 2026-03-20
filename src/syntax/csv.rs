use egui::{Color32, FontId, TextFormat};
use syntect::util::LinesWithEndings;

use crate::theme::ThemeKind;

/// Highlight CSV (or TSV) by cycling column colors.
pub fn highlight_csv_job(text: &str, theme_kind: ThemeKind, separator: u8) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font_id = FontId::monospace(13.0);
    let colors: &[Color32] = match theme_kind {
        ThemeKind::Dark => &[
            Color32::from_rgb(220, 225, 232), // default text
            Color32::from_rgb(110, 179, 255), // blue
            Color32::from_rgb(143, 219, 173), // green
            Color32::from_rgb(255, 206, 129), // yellow
            Color32::from_rgb(198, 120, 221), // purple
        ],
        ThemeKind::Light => &[
            Color32::from_rgb(40, 45, 55),
            Color32::from_rgb(25, 86, 178),
            Color32::from_rgb(20, 128, 92),
            Color32::from_rgb(170, 110, 20),
            Color32::from_rgb(140, 40, 160),
        ],
    };
    let sep_color = match theme_kind {
        ThemeKind::Dark => Color32::from_rgb(120, 132, 150),
        ThemeKind::Light => Color32::from_rgb(120, 120, 120),
    };

    for line in LinesWithEndings::from(text) {
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut col_idx = 0;

        while i < bytes.len() {
            let ch = bytes[i];

            // Separator
            if ch == separator {
                job.append(
                    &line[i..i + 1],
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color: sep_color,
                        ..Default::default()
                    },
                );
                i += 1;
                col_idx += 1;
                continue;
            }

            // Quoted field
            if ch == b'"' {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        i += 1;
                        // Escaped quote ""
                        if i < bytes.len() && bytes[i] == b'"' {
                            i += 1;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                let color = colors[col_idx % colors.len()];
                job.append(
                    &line[start..i],
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color,
                        ..Default::default()
                    },
                );
                continue;
            }

            // Newline / carriage return
            if ch == b'\n' || ch == b'\r' {
                let start = i;
                while i < bytes.len() && (bytes[i] == b'\n' || bytes[i] == b'\r') {
                    i += 1;
                }
                job.append(
                    &line[start..i],
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color: colors[0],
                        ..Default::default()
                    },
                );
                continue;
            }

            // Unquoted field content
            let start = i;
            while i < bytes.len() && bytes[i] != separator && bytes[i] != b'"' && bytes[i] != b'\n' && bytes[i] != b'\r' {
                i += 1;
            }
            let color = colors[col_idx % colors.len()];
            job.append(
                &line[start..i],
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color,
                    ..Default::default()
                },
            );
        }
    }

    job
}
