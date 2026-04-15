use std::path::PathBuf;

use png::Compression;
use zune_core::colorspace::ColorSpace;
use zune_image::image::Image as ZuneImage;

pub struct SnapshotDiff {
    pub mismatched: u64,
    pub total: u64,
    pub max_channel_diff: u8,
    pub fraction: f32,
}

pub fn compare_snapshots(
    actual_path: &PathBuf,
    expected_path: &PathBuf,
    max_channel_diff: u8,
    max_pixel_fraction: f32,
) -> Result<SnapshotDiff, String> {
    let (actual_w, actual_h, actual) = decode_rgba(actual_path)?;
    let (expected_w, expected_h, expected) = decode_rgba(expected_path)?;

    if actual_w != expected_w || actual_h != expected_h {
        return Err(format!(
            "Image dimensions differ: actual {}x{}, expected {}x{}",
            actual_w, actual_h, expected_w, expected_h
        ));
    }

    let mut mismatched = 0u64;
    let mut max_seen_diff: u8 = 0;
    for (a, e) in actual.chunks_exact(4).zip(expected.chunks_exact(4)) {
        let mut pixel_diff = 0u8;
        for channel in 0..4 {
            let diff = a[channel].abs_diff(e[channel]);
            pixel_diff = pixel_diff.max(diff);
        }
        max_seen_diff = max_seen_diff.max(pixel_diff);
        if pixel_diff > max_channel_diff {
            mismatched += 1;
        }
    }

    let total = actual_w as u64 * actual_h as u64;
    let fraction = mismatched as f32 / total as f32;
    if fraction > max_pixel_fraction {
        return Err(format!(
            "Too many mismatched pixels: {:.6} > {:.6}",
            fraction, max_pixel_fraction
        ));
    }

    Ok(SnapshotDiff {
        mismatched,
        total,
        max_channel_diff: max_seen_diff,
        fraction,
    })
}

pub fn align_to(value: u32, alignment: u32) -> u32 {
    value.div_ceil(alignment) * alignment
}

pub fn save_snapshot_png(
    buffer: &blade_graphics::Buffer,
    width: u32,
    height: u32,
    bytes_per_row: usize,
    path: &PathBuf,
) -> Result<(), String> {
    let row_bytes = (width * 4) as usize;
    let mut data = vec![0u8; row_bytes * height as usize];
    let src = buffer.data() as *const u8;
    for y in 0..height as usize {
        let src_row = unsafe { std::slice::from_raw_parts(src.add(y * bytes_per_row), row_bytes) };
        let dst_row = &mut data[y * row_bytes..(y + 1) * row_bytes];
        dst_row.copy_from_slice(src_row);
    }

    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for chunk in data.chunks_exact(4) {
        rgb.push(chunk[0]);
        rgb.push(chunk[1]);
        rgb.push(chunk[2]);
    }
    let file = std::fs::File::create(path).map_err(|err| format!("Failed to create PNG: {err}"))?;
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(Compression::High);
    let mut writer = encoder
        .write_header()
        .map_err(|err| format!("Failed to write PNG header: {err}"))?;
    writer
        .write_image_data(&rgb)
        .map_err(|err| format!("Failed to write PNG data: {err}"))?;
    Ok(())
}

fn decode_rgba(path: &PathBuf) -> Result<(usize, usize, Vec<u8>), String> {
    let image = ZuneImage::open(path).map_err(|err| format!("Failed to open image: {err:?}"))?;
    let (width, height) = image.dimensions();
    let colorspace = image.colorspace();
    let mut frames = image.flatten_to_u8();
    let data = frames
        .pop()
        .ok_or_else(|| "Missing image data".to_string())?;
    let rgba = convert_to_rgba(&data, width, height, colorspace)?;
    Ok((width, height, rgba))
}

fn convert_to_rgba(
    data: &[u8],
    width: usize,
    height: usize,
    colorspace: ColorSpace,
) -> Result<Vec<u8>, String> {
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| "Invalid dimensions".to_string())?;
    match colorspace {
        ColorSpace::RGBA => {
            if data.len() == pixels * 4 {
                Ok(data.to_vec())
            } else {
                Err("Unexpected RGBA buffer length".to_string())
            }
        }
        ColorSpace::RGB => {
            if data.len() != pixels * 3 {
                return Err("Unexpected RGB buffer length".to_string());
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(3) {
                out.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            Ok(out)
        }
        ColorSpace::BGR => {
            if data.len() != pixels * 3 {
                return Err("Unexpected BGR buffer length".to_string());
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(3) {
                out.extend_from_slice(&[chunk[2], chunk[1], chunk[0], 255]);
            }
            Ok(out)
        }
        ColorSpace::BGRA => {
            if data.len() != pixels * 4 {
                return Err("Unexpected BGRA buffer length".to_string());
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(4) {
                out.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
            }
            Ok(out)
        }
        ColorSpace::ARGB => {
            if data.len() != pixels * 4 {
                return Err("Unexpected ARGB buffer length".to_string());
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(4) {
                out.extend_from_slice(&[chunk[1], chunk[2], chunk[3], chunk[0]]);
            }
            Ok(out)
        }
        ColorSpace::Luma => {
            if data.len() != pixels {
                return Err("Unexpected Luma buffer length".to_string());
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for &v in data {
                out.extend_from_slice(&[v, v, v, 255]);
            }
            Ok(out)
        }
        ColorSpace::LumaA => {
            if data.len() != pixels * 2 {
                return Err("Unexpected LumaA buffer length".to_string());
            }
            let mut out = Vec::with_capacity(pixels * 4);
            for chunk in data.chunks_exact(2) {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            Ok(out)
        }
        _ => Err(format!("Unsupported colorspace: {colorspace:?}")),
    }
}
