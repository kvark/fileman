use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use fileman::{app_state, core, theme};
use zune_core;

use crate::{
    HighlightRequest, ImageCache, ImageRequest, ImageSource, color32, hash_text, hexdump_job,
    make_whitespace_visible, mark_line_endings, touch_image,
};

pub struct PreviewRender<'a> {
    pub theme: &'a theme::Theme,
    pub is_focused: bool,
    pub preview: &'a mut app_state::PreviewState,
    pub image_cache: &'a mut ImageCache,
    pub image_req_tx: &'a mpsc::Sender<ImageRequest>,
    pub highlight_cache: &'a HashMap<String, egui::text::LayoutJob>,
    pub highlight_pending: &'a mut HashSet<String>,
    pub highlight_req_tx: &'a mpsc::Sender<HighlightRequest>,
    pub min_height: f32,
}

pub fn draw_preview(ui: &mut egui::Ui, ctx: PreviewRender<'_>) {
    let PreviewRender {
        theme,
        is_focused,
        preview,
        image_cache,
        image_req_tx,
        highlight_cache,
        highlight_pending,
        highlight_req_tx,
        min_height,
    } = ctx;
    let colors = theme.colors();
    let header_bg = color32(colors.preview_header_bg);
    let header_fg = color32(colors.preview_header_fg);
    let text_color = color32(colors.preview_text);

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
            egui::Frame::NONE.fill(header_bg).show(ui, |ui| {
                if is_focused {
                    ui.colored_label(header_fg, "● Preview (Tab to return)");
                } else {
                    ui.colored_label(header_fg, "Preview (Tab to focus)");
                }
            });
            ui.add_space(4.0);

            if let Some(core::PreviewContent::Text(_)) = preview.content.as_ref() {
                if preview.find_open {
                    ui.horizontal(|ui| {
                        ui.colored_label(text_color, "Find:");
                        let response = ui.text_edit_singleline(&mut preview.find_query);
                        if preview.find_focus {
                            response.request_focus();
                            preview.find_focus = false;
                        }
                    });
                    ui.add_space(4.0);
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut preview.wrap, "Wrap");
                    ui.checkbox(&mut preview.show_whitespace, "Show whitespace");
                });
                ui.add_space(6.0);
            } else if let Some(core::PreviewContent::Binary(_)) = preview.content.as_ref() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut preview.bytes_per_row_auto, "Auto bytes/row");
                    if !preview.bytes_per_row_auto {
                        ui.add(
                            egui::Slider::new(&mut preview.bytes_per_row, 4..=32)
                                .step_by(4.0)
                                .text("bytes/row"),
                        );
                    }
                });
                ui.add_space(6.0);
            }

            let page_height = ui.available_height();
            let output = ui
                .scope_builder(
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        ui.available_rect_before_wrap().min,
                        egui::Vec2::new(ui.available_width(), page_height),
                    )),
                    |ui| {
                        let scroll = if preview.wrap {
                            egui::ScrollArea::vertical()
                        } else {
                            egui::ScrollArea::both()
                        };
                        scroll
                            .auto_shrink([false, false])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                            )
                            .vertical_scroll_offset(preview.scroll)
                            .show(ui, |ui| match preview.content.as_ref() {
                                Some(core::PreviewContent::Text(text))
                                | Some(core::PreviewContent::TextChunk { text, .. }) => {
                                    let display_text = if preview.show_whitespace {
                                        make_whitespace_visible(text)
                                    } else if preview.wrap {
                                        mark_line_endings(text)
                                    } else {
                                        text.clone()
                                    };

                                    let ext = preview.ext.clone();
                                    let base_key = preview
                                        .key
                                        .clone()
                                        .unwrap_or_else(|| "unknown".to_string());
                                    let key = format!("{base_key}:{:x}", hash_text(&display_text));
                                    let wrap_width = if preview.wrap {
                                        ui.available_width()
                                    } else {
                                        f32::INFINITY
                                    };
                                    if let Some(job) = highlight_cache.get(&key) {
                                        let mut job = job.clone();
                                        job.wrap.max_width = wrap_width;
                                        job.wrap.break_anywhere = preview.wrap;
                                        let label = egui::Label::new(job)
                                            .selectable(true)
                                            .wrap_mode(if preview.wrap {
                                                egui::TextWrapMode::Wrap
                                            } else {
                                                egui::TextWrapMode::Extend
                                            });
                                        ui.add(label);
                                    } else {
                                        ui.horizontal(|ui| {
                                            ui.add(egui::Spinner::new());
                                            ui.colored_label(text_color, "Highlighting…");
                                        });
                                        ui.add_space(6.0);
                                        if highlight_pending.insert(key.clone()) {
                                            let _ = highlight_req_tx.send(HighlightRequest {
                                                key: key.clone(),
                                                text: display_text.clone(),
                                                ext,
                                                theme_kind: theme.kind,
                                            });
                                        }
                                        let mut job = egui::text::LayoutJob::simple(
                                            display_text.clone(),
                                            egui::TextStyle::Monospace.resolve(ui.style()),
                                            egui::Color32::LIGHT_GRAY,
                                            wrap_width,
                                        );
                                        job.wrap.break_anywhere = preview.wrap;
                                        let label = egui::Label::new(job)
                                            .selectable(true)
                                            .wrap_mode(if preview.wrap {
                                                egui::TextWrapMode::Wrap
                                            } else {
                                                egui::TextWrapMode::Extend
                                            });
                                        ui.add(label);
                                    }
                                }
                                Some(core::PreviewContent::Binary(bytes))
                                | Some(core::PreviewContent::BinaryChunk { data: bytes, .. }) => {
                                    let width = if preview.bytes_per_row_auto {
                                        let mut best = 4usize;
                                        let options = [4usize, 8, 12, 16, 20, 24, 28, 32];
                                        let font = egui::TextStyle::Monospace.resolve(ui.style());
                                        for opt in options {
                                            let sample = core::hexdump_with_width(
                                                &bytes[..bytes.len().min(opt)],
                                                opt,
                                            );
                                            let sample = sample.lines().next().unwrap_or_default();
                                            let w = ui
                                                .painter()
                                                .layout_no_wrap(
                                                    sample.to_string(),
                                                    font.clone(),
                                                    text_color,
                                                )
                                                .size()
                                                .x;
                                            if w <= ui.available_width() {
                                                best = opt;
                                            } else {
                                                break;
                                            }
                                        }
                                        preview.bytes_per_row = best;
                                        best
                                    } else {
                                        preview.bytes_per_row
                                    };
                                    let job = hexdump_job(bytes, width, &colors, ui);
                                    ui.add(egui::Label::new(job).selectable(true));
                                }
                                Some(core::PreviewContent::Image(path)) => {
                                    let (key, request) = match path {
                                        core::ImageLocation::Fs(path) => {
                                            let key = path.to_string_lossy().into_owned();
                                            (
                                                key.clone(),
                                                ImageRequest {
                                                    key,
                                                    source: ImageSource::Fs(
                                                        path.as_ref().to_path_buf(),
                                                    ),
                                                },
                                            )
                                        }
                                        core::ImageLocation::Container {
                                            kind,
                                            archive_path,
                                            inner_path,
                                        } => {
                                            let key = format!(
                                                "{}::{}:/{}",
                                                archive_path.to_string_lossy(),
                                                match kind {
                                                    core::ContainerKind::Zip => "zip",
                                                    core::ContainerKind::Tar => "tar",
                                                    core::ContainerKind::TarGz => "tar.gz",
                                                    core::ContainerKind::TarBz2 => "tar.bz2",
                                                },
                                                inner_path
                                            );
                                            (
                                                key.clone(),
                                                ImageRequest {
                                                    key,
                                                    source: ImageSource::Container {
                                                        kind: *kind,
                                                        archive_path: archive_path.clone(),
                                                        inner_path: inner_path.clone(),
                                                    },
                                                },
                                            )
                                        }
                                    };
                                    if let Some(message) = image_cache.failures.get(&key) {
                                        ui.colored_label(
                                            text_color,
                                            format!("Failed to decode image\n{message}"),
                                        );
                                    } else if let Some(mut handle) =
                                        image_cache.textures.get(&key).cloned()
                                    {
                                        touch_image(image_cache, &key);
                                        // Animate GIF: update texture to current frame
                                        if let Some(anim) = image_cache.animations.get(&key) {
                                            let time_ms =
                                                (ui.ctx().input(|i| i.time) * 1000.0) as u64;
                                            let loop_pos = time_ms % anim.total_duration_ms;
                                            let mut acc = 0u64;
                                            let mut frame_idx = 0;
                                            for (i, delay) in anim.delays.iter().enumerate() {
                                                acc += *delay as u64;
                                                if loop_pos < acc {
                                                    frame_idx = i;
                                                    break;
                                                }
                                            }
                                            handle.set(
                                                anim.frames[frame_idx].clone(),
                                                egui::TextureOptions::LINEAR,
                                            );
                                            let remaining = acc.saturating_sub(loop_pos).max(10);
                                            ui.ctx().request_repaint_after(
                                                std::time::Duration::from_millis(remaining),
                                            );
                                        }
                                        if let Some(meta) = image_cache.meta.get(&key) {
                                            let depth_bits = match meta.depth {
                                                zune_core::bit_depth::BitDepth::Eight => "8-bit",
                                                zune_core::bit_depth::BitDepth::Sixteen => "16-bit",
                                                zune_core::bit_depth::BitDepth::Float32 => "32-bit",
                                                _ => "unknown",
                                            };
                                            let anim_info = if let Some(anim) =
                                                image_cache.animations.get(&key)
                                            {
                                                format!(" · {} frames", anim.frames.len())
                                            } else {
                                                String::new()
                                            };
                                            ui.colored_label(
                                                text_color,
                                                format!(
                                                    "{}×{} · {}{}",
                                                    meta.width, meta.height, depth_bits, anim_info
                                                ),
                                            );
                                            ui.add_space(6.0);
                                        }
                                        let is_refining = image_cache.refining.contains_key(&key);
                                        let is_animated = image_cache.animations.contains_key(&key);
                                        if is_refining {
                                            let t = ui.ctx().input(|i| i.time);
                                            let spinner =
                                                ["|", "/", "-", "\\"][((t * 3.0) as usize) % 4];
                                            ui.colored_label(
                                                text_color,
                                                format!("{spinner} Loading full image..."),
                                            );
                                            ui.ctx().request_repaint_after(
                                                std::time::Duration::from_millis(333),
                                            );
                                        }
                                        let sized = egui::load::SizedTexture::from_handle(&handle);
                                        let available = ui.available_size();
                                        let tex = sized.size;
                                        let fit_scale = (available.x / tex.x)
                                            .min(available.y / tex.y)
                                            .max(0.01);

                                        // Zoom slider: only for non-animated, fully loaded images
                                        let old_scale = if preview.image_zoom == 0.0
                                            || is_refining
                                            || is_animated
                                        {
                                            fit_scale
                                        } else {
                                            preview.image_zoom
                                        };
                                        if !is_refining && !is_animated {
                                            ui.horizontal(|ui| {
                                                ui.colored_label(text_color, "Zoom");
                                                ui.spacing_mut().slider_width =
                                                    (available.x - 120.0).max(60.0);
                                                let min_zoom = fit_scale.max(0.01);
                                                let mut display_zoom = if preview.image_zoom == 0.0
                                                {
                                                    min_zoom
                                                } else {
                                                    preview.image_zoom
                                                };
                                                let slider = egui::Slider::new(
                                                    &mut display_zoom,
                                                    min_zoom..=5.0,
                                                )
                                                .logarithmic(true)
                                                .custom_formatter(move |v, _| {
                                                    if (v - min_zoom as f64).abs() < 0.001 {
                                                        "Fit".to_string()
                                                    } else {
                                                        format!("{:.0}%", v * 100.0)
                                                    }
                                                })
                                                .custom_parser(|s| {
                                                    let s = s.trim().trim_end_matches('%');
                                                    s.parse::<f64>().ok().map(|v| v / 100.0)
                                                });
                                                let resp = ui.add(slider);
                                                if resp.changed() {
                                                    if (display_zoom - min_zoom).abs() < 0.001 {
                                                        preview.image_zoom = 0.0;
                                                    } else {
                                                        preview.image_zoom = display_zoom;
                                                    }
                                                }
                                            });
                                            ui.add_space(4.0);
                                        }

                                        let new_scale = if preview.image_zoom == 0.0
                                            || is_refining
                                            || is_animated
                                        {
                                            fit_scale
                                        } else {
                                            preview.image_zoom
                                        };
                                        let viewport = ui.available_size();

                                        // Anchor zoom to center of visible area
                                        if (new_scale - old_scale).abs() > 0.001 {
                                            let old_size_x = tex.x * old_scale;
                                            let old_size_y = tex.y * old_scale;
                                            let cx = if old_size_x > viewport.x {
                                                (preview.image_pan[0] + viewport.x * 0.5)
                                                    / old_size_x
                                            } else {
                                                0.5
                                            };
                                            let cy = if old_size_y > viewport.y {
                                                (preview.image_pan[1] + viewport.y * 0.5)
                                                    / old_size_y
                                            } else {
                                                0.5
                                            };
                                            let new_size_x = tex.x * new_scale;
                                            let new_size_y = tex.y * new_scale;
                                            preview.image_pan[0] = (cx * new_size_x
                                                - viewport.x * 0.5)
                                                .clamp(0.0, (new_size_x - viewport.x).max(0.0));
                                            preview.image_pan[1] = (cy * new_size_y
                                                - viewport.y * 0.5)
                                                .clamp(0.0, (new_size_y - viewport.y).max(0.0));
                                        }

                                        let size =
                                            egui::Vec2::new(tex.x * new_scale, tex.y * new_scale);

                                        if size.x > viewport.x || size.y > viewport.y {
                                            // Zoomed beyond panel: show scrollable area
                                            let scroll_area = egui::ScrollArea::both()
                                                .auto_shrink([false, false])
                                                .scroll_offset(egui::Vec2::new(
                                                    preview.image_pan[0],
                                                    preview.image_pan[1],
                                                ));
                                            let output = scroll_area.show(ui, |ui| {
                                                ui.add(
                                                    egui::Image::new(sized).fit_to_exact_size(size),
                                                );
                                            });
                                            preview.image_pan[0] = output.state.offset.x;
                                            preview.image_pan[1] = output.state.offset.y;
                                        } else {
                                            preview.image_pan = [0.0, 0.0];
                                            ui.add(egui::Image::new(sized).fit_to_exact_size(size));
                                        }
                                        ui.ctx().request_repaint();
                                    } else {
                                        if image_cache.pending.insert(key.clone()) {
                                            let _ = image_req_tx.send(request);
                                        }
                                        let t = ui.ctx().input(|i| i.time);
                                        let spinner =
                                            ["|", "/", "-", "\\"][((t * 3.0) as usize) % 4];
                                        let mono = egui::TextStyle::Monospace.resolve(ui.style());
                                        let body = egui::TextStyle::Body.resolve(ui.style());
                                        let mut job = egui::text::LayoutJob::default();
                                        let fmt_mono = egui::text::TextFormat {
                                            font_id: mono,
                                            color: text_color,
                                            ..Default::default()
                                        };
                                        let fmt_body = egui::text::TextFormat {
                                            font_id: body,
                                            color: text_color,
                                            ..Default::default()
                                        };
                                        job.append(spinner, 0.0, fmt_mono);
                                        job.append(
                                            &format!(" Loading image...\n{}", key),
                                            0.0,
                                            fmt_body,
                                        );
                                        ui.label(job);
                                        ui.ctx().request_repaint_after(
                                            std::time::Duration::from_millis(333),
                                        );
                                    }
                                }
                                None => {
                                    if preview.loading_since.is_some() {
                                        let t = ui.ctx().input(|i| i.time);
                                        let spinner =
                                            ["|", "/", "-", "\\"][((t * 3.0) as usize) % 4];
                                        let mono = egui::TextStyle::Monospace.resolve(ui.style());
                                        let body = egui::TextStyle::Body.resolve(ui.style());
                                        let mut job = egui::text::LayoutJob::default();
                                        let fmt_mono = egui::text::TextFormat {
                                            font_id: mono,
                                            color: text_color,
                                            ..Default::default()
                                        };
                                        let fmt_body = egui::text::TextFormat {
                                            font_id: body,
                                            color: text_color,
                                            ..Default::default()
                                        };
                                        job.append(spinner, 0.0, fmt_mono);
                                        job.append(" Loading preview...", 0.0, fmt_body);
                                        ui.label(job);
                                    } else {
                                        ui.colored_label(text_color, "No preview");
                                    }
                                }
                            })
                    },
                )
                .inner;
            preview.scroll = output.state.offset.y;
            preview.page_height = page_height;
            preview.line_height = ui.text_style_height(&egui::TextStyle::Body);
            preview.max_scroll = (output.content_size.y - output.inner_rect.height()).max(0.0);
            preview.can_scroll = output.content_size.y > output.inner_rect.height();
        });
}
