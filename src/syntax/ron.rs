use egui::{Color32, FontId, TextFormat};
use syntect::util::LinesWithEndings;

use crate::theme::ThemeKind;

const KEYWORDS: &[&str] = &["true", "false", "Some", "None", "Optional"];

pub fn highlight_ron_job(text: &str, theme_kind: ThemeKind) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font_id = FontId::monospace(13.0);
    let (keyword_color, string_color, number_color, comment_color, struct_color, text_color) =
        match theme_kind {
            ThemeKind::Dark => (
                Color32::from_rgb(198, 120, 221), // purple
                Color32::from_rgb(143, 219, 173), // green
                Color32::from_rgb(255, 206, 129), // yellow
                Color32::from_rgb(120, 132, 150), // grey
                Color32::from_rgb(110, 179, 255), // blue
                Color32::from_rgb(220, 225, 232),
            ),
            ThemeKind::Light => (
                Color32::from_rgb(140, 40, 160),
                Color32::from_rgb(20, 128, 92),
                Color32::from_rgb(170, 110, 20),
                Color32::from_rgb(120, 120, 120),
                Color32::from_rgb(25, 86, 178),
                Color32::from_rgb(40, 45, 55),
            ),
        };

    for line in LinesWithEndings::from(text) {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let ch = bytes[i];
            // Line comment
            if ch == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                job.append(
                    &line[i..],
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color: comment_color,
                        ..Default::default()
                    },
                );
                i = bytes.len();
                continue;
            }
            // Block comment
            if ch == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                let start = i;
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                if i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i = bytes.len();
                }
                job.append(
                    &line[start..i],
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color: comment_color,
                        ..Default::default()
                    },
                );
                continue;
            }
            // String
            if ch == b'"' {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i = (i + 2).min(bytes.len());
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                job.append(
                    &line[start..i],
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color: string_color,
                        ..Default::default()
                    },
                );
                continue;
            }
            // Character literal
            if ch == b'\'' {
                let start = i;
                i += 1;
                if i < bytes.len() && bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                }
                if i < bytes.len() {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'\'' {
                    i += 1;
                }
                job.append(
                    &line[start..i],
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color: string_color,
                        ..Default::default()
                    },
                );
                continue;
            }
            // Number
            if ch.is_ascii_digit()
                || (ch == b'-'
                    && i + 1 < bytes.len()
                    && bytes[i + 1].is_ascii_digit()
                    && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()))
            {
                let start = i;
                if ch == b'-' {
                    i += 1;
                }
                while i < bytes.len() {
                    let c = bytes[i];
                    if c.is_ascii_digit() || c == b'.' || c == b'_' || c == b'x' || c == b'o' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                job.append(
                    &line[start..i],
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color: number_color,
                        ..Default::default()
                    },
                );
                continue;
            }
            // Identifier / keyword / struct name
            if ch.is_ascii_alphabetic() || ch == b'_' {
                let start = i;
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = &line[start..i];
                let color = if KEYWORDS.contains(&word) {
                    keyword_color
                } else if i < bytes.len() && bytes[i] == b'(' {
                    // Struct/enum name followed by (
                    struct_color
                } else {
                    text_color
                };
                job.append(
                    word,
                    0.0,
                    TextFormat {
                        font_id: font_id.clone(),
                        color,
                        ..Default::default()
                    },
                );
                continue;
            }
            // Everything else (punctuation, whitespace)
            let start = i;
            i += 1;
            job.append(
                &line[start..i],
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: text_color,
                    ..Default::default()
                },
            );
        }
    }

    job
}
