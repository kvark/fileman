use std::io;

pub struct ImageMeta {
    pub width: usize,
    pub height: usize,
    pub depth: zune_core::bit_depth::BitDepth,
}

pub struct GifFrame {
    pub image: egui::ColorImage,
    pub delay_ms: u32,
}

pub enum DecodedImage {
    Static(egui::ColorImage),
    Animated(Vec<GifFrame>),
}

pub fn is_gif(bytes: &[u8]) -> bool {
    bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"))
}

pub fn decode_gif_first_frame(
    bytes: &[u8],
    max_side: u32,
) -> Option<(egui::ColorImage, ImageMeta)> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let cursor = io::Cursor::new(bytes);
    let mut decoder = options.read_info(cursor).ok()?;
    let screen_w = decoder.width() as usize;
    let screen_h = decoder.height() as usize;
    if screen_w == 0 || screen_h == 0 {
        return None;
    }
    let frame = decoder.read_next_frame().ok()??;
    let mut canvas = vec![0u8; screen_w * screen_h * 4];
    let fw = frame.width as usize;
    let fh = frame.height as usize;
    let fl = frame.left as usize;
    let ft = frame.top as usize;
    for y in 0..fh {
        for x in 0..fw {
            let cx = fl + x;
            let cy = ft + y;
            if cx >= screen_w || cy >= screen_h {
                continue;
            }
            let src = (y * fw + x) * 4;
            if src + 3 >= frame.buffer.len() {
                continue;
            }
            if frame.buffer[src + 3] > 0 {
                let dst = (cy * screen_w + cx) * 4;
                canvas[dst..dst + 4].copy_from_slice(&frame.buffer[src..src + 4]);
            }
        }
    }
    let (out_w, out_h, out_rgba) = downscale_rgba(&canvas, screen_w, screen_h, max_side);
    let color = egui::ColorImage::from_rgba_unmultiplied([out_w, out_h], &out_rgba);
    Some((
        color,
        ImageMeta {
            width: screen_w,
            height: screen_h,
            depth: zune_core::bit_depth::BitDepth::Eight,
        },
    ))
}

pub fn decode_image_bytes(bytes: &[u8], max_side: u32) -> Option<(DecodedImage, ImageMeta)> {
    // Try GIF first (before zune, which only decodes the first frame)
    if is_gif(bytes) {
        return decode_gif_bytes(bytes, max_side);
    }

    let options = zune_core::options::DecoderOptions::new_fast();
    if let Ok(image) = zune_image::image::Image::read(bytes, options) {
        let orientation = exif_orientation(&image).unwrap_or(1);
        let (width, height) = image.dimensions();
        let depth = image.depth();
        let colorspace = image.colorspace();
        let mut frames = image.flatten_to_u8();
        let data = frames.pop()?;
        let rgba = convert_to_rgba(&data, width, height, colorspace)?;
        let (rgba, width, height) = apply_orientation_rgba(rgba, width, height, orientation);
        let (out_w, out_h, out_rgba) = downscale_rgba(&rgba, width, height, max_side);
        let color = egui::ColorImage::from_rgba_unmultiplied([out_w, out_h], &out_rgba);
        return Some((
            DecodedImage::Static(color),
            ImageMeta {
                width,
                height,
                depth,
            },
        ));
    }

    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return decode_webp_bytes(bytes, max_side);
    }

    if bytes.len() >= 2 && bytes[0] == b'B' && bytes[1] == b'M' {
        return decode_bmp_bytes(bytes, max_side);
    }

    None
}

fn decode_gif_bytes(bytes: &[u8], max_side: u32) -> Option<(DecodedImage, ImageMeta)> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let cursor = io::Cursor::new(bytes);
    let mut decoder = options.read_info(cursor).ok()?;
    let screen_w = decoder.width() as usize;
    let screen_h = decoder.height() as usize;
    if screen_w == 0 || screen_h == 0 {
        return None;
    }

    let mut canvas = vec![0u8; screen_w * screen_h * 4];
    let mut frames = Vec::new();

    while let Ok(Some(frame)) = decoder.read_next_frame() {
        let saved = canvas.clone();

        // Composite frame onto canvas
        let fw = frame.width as usize;
        let fh = frame.height as usize;
        let fl = frame.left as usize;
        let ft = frame.top as usize;
        for y in 0..fh {
            for x in 0..fw {
                let cx = fl + x;
                let cy = ft + y;
                if cx >= screen_w || cy >= screen_h {
                    continue;
                }
                let src = (y * fw + x) * 4;
                if src + 3 >= frame.buffer.len() {
                    continue;
                }
                let alpha = frame.buffer[src + 3];
                if alpha > 0 {
                    let dst = (cy * screen_w + cx) * 4;
                    canvas[dst..dst + 4].copy_from_slice(&frame.buffer[src..src + 4]);
                }
            }
        }

        let (out_w, out_h, out_rgba) = downscale_rgba(&canvas, screen_w, screen_h, max_side);
        let color = egui::ColorImage::from_rgba_unmultiplied([out_w, out_h], &out_rgba);
        // GIF delays are in centiseconds; 0 means use default (~100ms)
        let delay_cs = u32::from(frame.delay);
        let delay_ms = if delay_cs == 0 { 100 } else { delay_cs * 10 };
        frames.push(GifFrame {
            image: color,
            delay_ms,
        });

        // Apply disposal method
        match frame.dispose {
            gif::DisposalMethod::Keep | gif::DisposalMethod::Any => {}
            gif::DisposalMethod::Background => {
                for y in 0..fh {
                    for x in 0..fw {
                        let cx = fl + x;
                        let cy = ft + y;
                        if cx < screen_w && cy < screen_h {
                            let dst = (cy * screen_w + cx) * 4;
                            canvas[dst..dst + 4].copy_from_slice(&[0, 0, 0, 0]);
                        }
                    }
                }
            }
            gif::DisposalMethod::Previous => {
                canvas = saved;
            }
        }

        if frames.len() >= 200 {
            break;
        }
    }

    if frames.is_empty() {
        return None;
    }

    let meta = ImageMeta {
        width: screen_w,
        height: screen_h,
        depth: zune_core::bit_depth::BitDepth::Eight,
    };

    if frames.len() == 1 {
        let frame = frames.into_iter().next().unwrap();
        Some((DecodedImage::Static(frame.image), meta))
    } else {
        Some((DecodedImage::Animated(frames), meta))
    }
}

fn decode_webp_bytes(bytes: &[u8], max_side: u32) -> Option<(DecodedImage, ImageMeta)> {
    let cursor = io::Cursor::new(bytes);
    let mut decoder = image_webp::WebPDecoder::new(cursor).ok()?;
    let size = decoder.output_buffer_size()?;
    let mut data = vec![0u8; size];
    decoder.read_image(&mut data).ok()?;
    let (width, height) = decoder.dimensions();
    let width = width as usize;
    let height = height as usize;
    let has_alpha = decoder.has_alpha();
    let rgba = if has_alpha {
        data
    } else {
        let mut out = Vec::with_capacity(width * height * 4);
        for rgb in data.chunks_exact(3) {
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        out
    };
    let (out_w, out_h, out_rgba) = downscale_rgba(&rgba, width, height, max_side);
    let color = egui::ColorImage::from_rgba_unmultiplied([out_w, out_h], &out_rgba);
    Some((
        DecodedImage::Static(color),
        ImageMeta {
            width,
            height,
            depth: zune_core::bit_depth::BitDepth::Eight,
        },
    ))
}

fn decode_bmp_bytes(bytes: &[u8], max_side: u32) -> Option<(DecodedImage, ImageMeta)> {
    let mut decoder = zune_bmp::BmpDecoder::new(bytes);
    decoder.decode_headers().ok()?;
    let (width, height) = decoder.get_dimensions()?;
    let depth = decoder.get_depth();
    let colorspace = decoder.get_colorspace()?;
    let data = decoder.decode().ok()?;
    let rgba = convert_to_rgba(&data, width, height, colorspace)?;
    let (out_w, out_h, out_rgba) = downscale_rgba(&rgba, width, height, max_side);
    let color = egui::ColorImage::from_rgba_unmultiplied([out_w, out_h], &out_rgba);
    Some((
        DecodedImage::Static(color),
        ImageMeta {
            width,
            height,
            depth,
        },
    ))
}

fn exif_orientation(image: &zune_image::image::Image) -> Option<u16> {
    let exif = image.metadata().exif()?;
    for field in exif {
        if field.tag == exif::Tag::Orientation
            && let exif::Value::Short(values) = &field.value
        {
            return values.first().copied();
        }
    }
    None
}

fn apply_orientation_rgba(
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    orientation: u16,
) -> (Vec<u8>, usize, usize) {
    match orientation {
        2 => (flip_horizontal(&rgba, width, height), width, height),
        3 => (rotate_180(&rgba, width, height), width, height),
        4 => (flip_vertical(&rgba, width, height), width, height),
        5 => (
            transpose_flip_horizontal(&rgba, width, height),
            height,
            width,
        ),
        6 => (rotate_90_cw(&rgba, width, height), height, width),
        7 => (transpose_flip_vertical(&rgba, width, height), height, width),
        8 => (rotate_90_ccw(&rgba, width, height), height, width),
        _ => (rgba, width, height),
    }
}

fn flip_horizontal(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst = (y * width + (width - 1 - x)) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn flip_vertical(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst = ((height - 1 - y) * width + x) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn rotate_180(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst = ((height - 1 - y) * width + (width - 1 - x)) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn rotate_90_cw(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst_x = height - 1 - y;
            let dst_y = x;
            let dst = (dst_y * height + dst_x) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn rotate_90_ccw(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst_x = y;
            let dst_y = width - 1 - x;
            let dst = (dst_y * height + dst_x) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn transpose_flip_horizontal(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst_x = height - 1 - y;
            let dst_y = width - 1 - x;
            let dst = (dst_y * height + dst_x) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn transpose_flip_vertical(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) * 4;
            let dst_x = y;
            let dst_y = x;
            let dst = (dst_y * height + dst_x) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

fn convert_to_rgba(
    data: &[u8],
    width: usize,
    height: usize,
    colorspace: zune_core::colorspace::ColorSpace,
) -> Option<Vec<u8>> {
    let pixels = width.checked_mul(height)?;
    match colorspace {
        zune_core::colorspace::ColorSpace::RGBA => {
            if data.len() == pixels * 4 {
                Some(data.to_vec())
            } else {
                None
            }
        }
        zune_core::colorspace::ColorSpace::RGB => {
            if data.len() != pixels * 3 {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(3) {
                out.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            Some(out)
        }
        zune_core::colorspace::ColorSpace::BGR => {
            if data.len() != pixels * 3 {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(3) {
                out.extend_from_slice(&[chunk[2], chunk[1], chunk[0], 255]);
            }
            Some(out)
        }
        zune_core::colorspace::ColorSpace::BGRA => {
            if data.len() != pixels * 4 {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(4) {
                out.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
            }
            Some(out)
        }
        zune_core::colorspace::ColorSpace::ARGB => {
            if data.len() != pixels * 4 {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(4) {
                out.extend_from_slice(&[chunk[1], chunk[2], chunk[3], chunk[0]]);
            }
            Some(out)
        }
        zune_core::colorspace::ColorSpace::Luma => {
            if data.len() != pixels {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for &v in data {
                out.extend_from_slice(&[v, v, v, 255]);
            }
            Some(out)
        }
        zune_core::colorspace::ColorSpace::LumaA => {
            if data.len() != pixels * 2 {
                return None;
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(2) {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            Some(out)
        }
        _ => None,
    }
}

fn downscale_rgba(
    rgba: &[u8],
    width: usize,
    height: usize,
    max_side: u32,
) -> (usize, usize, Vec<u8>) {
    let max_dim = width.max(height);
    if max_dim <= max_side as usize {
        return (width, height, rgba.to_vec());
    }
    let scale = max_side as f32 / max_dim as f32;
    let out_w = (width as f32 * scale).round().max(1.0) as usize;
    let out_h = (height as f32 * scale).round().max(1.0) as usize;
    let mut out = vec![0u8; out_w * out_h * 4];
    for y in 0..out_h {
        let src_y = y * height / out_h;
        for x in 0..out_w {
            let src_x = x * width / out_w;
            let src_idx = (src_y * width + src_x) * 4;
            let dst_idx = (y * out_w + x) * 4;
            out[dst_idx..dst_idx + 4].copy_from_slice(&rgba[src_idx..src_idx + 4]);
        }
    }
    (out_w, out_h, out)
}
