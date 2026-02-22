use egui::{Color32, FontId, TextFormat};
use syntect::util::LinesWithEndings;

use crate::theme::ThemeKind;

pub fn highlight_toml_job(text: &str, theme_kind: ThemeKind) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font_id = FontId::monospace(13.0);
    let (key_color, string_color, number_color, comment_color, text_color) = match theme_kind {
        ThemeKind::Dark => (
            Color32::from_rgb(110, 179, 255),
            Color32::from_rgb(143, 219, 173),
            Color32::from_rgb(255, 206, 129),
            Color32::from_rgb(120, 132, 150),
            Color32::from_rgb(220, 225, 232),
        ),
        ThemeKind::Light => (
            Color32::from_rgb(25, 86, 178),
            Color32::from_rgb(20, 128, 92),
            Color32::from_rgb(170, 110, 20),
            Color32::from_rgb(120, 120, 120),
            Color32::from_rgb(40, 45, 55),
        ),
    };

    for line in LinesWithEndings::from(text) {
        let mut in_string = false;
        let mut string_delim = '\0';
        let mut comment_start = None;
        for (i, ch) in line.char_indices() {
            if !in_string {
                if ch == '"' || ch == '\'' {
                    in_string = true;
                    string_delim = ch;
                } else if ch == '#' {
                    comment_start = Some(i);
                    break;
                }
            } else if ch == string_delim {
                in_string = false;
            }
        }

        let (code_part, comment_part) = match comment_start {
            Some(idx) => (&line[..idx], &line[idx..]),
            None => (line, ""),
        };

        let mut key_end = None;
        if let Some(eq_pos) = code_part.find('=') {
            key_end = Some(eq_pos);
        }

        if let Some(end) = key_end {
            let (key, rest) = code_part.split_at(end);
            job.append(
                key,
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: key_color,
                    ..Default::default()
                },
            );
            job.append(
                rest,
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: text_color,
                    ..Default::default()
                },
            );
        } else {
            job.append(
                code_part,
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: text_color,
                    ..Default::default()
                },
            );
        }

        if !comment_part.is_empty() {
            job.append(
                comment_part,
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: comment_color,
                    ..Default::default()
                },
            );
        }
    }

    // Second pass: highlight strings and numbers in-place by rebuilding if needed.
    if !text.contains('"') && !text.contains('\'') && !text.chars().any(|c| c.is_ascii_digit()) {
        return job;
    }

    let mut job2 = egui::text::LayoutJob::default();
    for line in LinesWithEndings::from(text) {
        let mut i = 0;
        let bytes = line.as_bytes();
        while i < bytes.len() {
            let ch = bytes[i] as char;
            if ch == '"' || ch == '\'' {
                let delim = ch;
                let start = i;
                i += 1;
                while i < bytes.len() {
                    let c = bytes[i] as char;
                    if c == '\\' {
                        i = (i + 2).min(bytes.len());
                        continue;
                    }
                    if c == delim {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                job2.append(
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
            if ch.is_ascii_digit() {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    let c = bytes[i] as char;
                    if c.is_ascii_digit() || c == '.' || c == '_' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                job2.append(
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
            let start = i;
            i += 1;
            while i < bytes.len() {
                let c = bytes[i] as char;
                if c == '"' || c == '\'' || c.is_ascii_digit() {
                    break;
                }
                i += 1;
            }
            job2.append(
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

    job2
}
