use egui::{Color32, FontId, TextFormat};
use syntect::util::LinesWithEndings;

use crate::theme::ThemeKind;

pub fn highlight_nix_job(text: &str, theme_kind: ThemeKind) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font_id = FontId::monospace(13.0);
    let (keyword_color, string_color, number_color, comment_color, text_color, builtin_color) =
        match theme_kind {
            ThemeKind::Dark => (
                Color32::from_rgb(110, 179, 255),
                Color32::from_rgb(143, 219, 173),
                Color32::from_rgb(255, 206, 129),
                Color32::from_rgb(120, 132, 150),
                Color32::from_rgb(220, 225, 232),
                Color32::from_rgb(171, 162, 255),
            ),
            ThemeKind::Light => (
                Color32::from_rgb(25, 86, 178),
                Color32::from_rgb(20, 128, 92),
                Color32::from_rgb(170, 110, 20),
                Color32::from_rgb(120, 120, 120),
                Color32::from_rgb(40, 45, 55),
                Color32::from_rgb(110, 90, 190),
            ),
        };

    let keywords = [
        "let", "in", "if", "then", "else", "with", "rec", "inherit", "assert", "or", "import",
        "try", "catch",
    ];
    let literals = ["true", "false", "null"];

    let mut in_multiline = false;

    for line in LinesWithEndings::from(text) {
        let mut i = 0;
        let bytes = line.as_bytes();
        while i < bytes.len() {
            if in_multiline {
                if let Some(end) = line[i..].find("''") {
                    let end_idx = i + end + 2;
                    append(&mut job, &line[i..end_idx], string_color, font_id.clone());
                    i = end_idx;
                    in_multiline = false;
                } else {
                    append(&mut job, &line[i..], string_color, font_id.clone());
                    i = bytes.len();
                }
                continue;
            }

            let ch = bytes[i] as char;
            if i + 1 < bytes.len() && bytes[i] == b'\'' && bytes[i + 1] == b'\'' {
                append(&mut job, "''", string_color, font_id.clone());
                i += 2;
                in_multiline = true;
                continue;
            }

            if ch == '"' {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    let c = bytes[i] as char;
                    if c == '\\' {
                        i = (i + 2).min(bytes.len());
                        continue;
                    }
                    if c == '"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                append(&mut job, &line[start..i], string_color, font_id.clone());
                continue;
            }

            if ch == '#' {
                append(&mut job, &line[i..], comment_color, font_id.clone());
                break;
            }

            if ch.is_ascii_digit() {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    let c = bytes[i] as char;
                    if c.is_ascii_digit() || c == '.' || c == '_' || c == 'x' || c == 'b' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                append(&mut job, &line[start..i], number_color, font_id.clone());
                continue;
            }

            if ch.is_ascii_alphabetic() || ch == '_' {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    let c = bytes[i] as char;
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                let word = &line[start..i];
                if keywords.contains(&word) || literals.contains(&word) {
                    append(&mut job, word, keyword_color, font_id.clone());
                } else if word == "builtins" {
                    append(&mut job, word, builtin_color, font_id.clone());
                } else {
                    append(&mut job, word, text_color, font_id.clone());
                }
                continue;
            }

            append(&mut job, &line[i..i + 1], text_color, font_id.clone());
            i += 1;
        }
    }

    job
}

fn append(job: &mut egui::text::LayoutJob, text: &str, color: Color32, font_id: FontId) {
    job.append(
        text,
        0.0,
        TextFormat {
            font_id,
            color,
            ..Default::default()
        },
    );
}
