use std::env;
use std::path::PathBuf;

use image::GenericImageView;

fn parse_args() -> Result<(PathBuf, PathBuf, u8, f32), String> {
    let mut args = env::args().skip(1);
    let actual = args
        .next()
        .ok_or_else(|| "Usage: snapshot_compare <actual> <expected> [--max-channel-diff N] [--max-pixel-fraction F]".to_string())?;
    let expected = args
        .next()
        .ok_or_else(|| "Usage: snapshot_compare <actual> <expected> [--max-channel-diff N] [--max-pixel-fraction F]".to_string())?;

    let mut max_channel_diff: u8 = 4;
    let mut max_pixel_fraction: f32 = 0.001;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--max-channel-diff" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--max-channel-diff requires a value".to_string())?;
                max_channel_diff = value
                    .parse::<u8>()
                    .map_err(|_| "Invalid --max-channel-diff value".to_string())?;
            }
            "--max-pixel-fraction" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--max-pixel-fraction requires a value".to_string())?;
                max_pixel_fraction = value
                    .parse::<f32>()
                    .map_err(|_| "Invalid --max-pixel-fraction value".to_string())?;
            }
            _ => return Err(format!("Unknown argument: {arg}")),
        }
    }

    Ok((
        PathBuf::from(actual),
        PathBuf::from(expected),
        max_channel_diff,
        max_pixel_fraction,
    ))
}

fn main() -> Result<(), String> {
    let (actual_path, expected_path, max_channel_diff, max_pixel_fraction) = parse_args()?;

    let actual = image::open(&actual_path)
        .map_err(|err| format!("Failed to open actual image: {err}"))?
        .to_rgba8();
    let expected = image::open(&expected_path)
        .map_err(|err| format!("Failed to open expected image: {err}"))?
        .to_rgba8();

    let (actual_w, actual_h) = actual.dimensions();
    let (expected_w, expected_h) = expected.dimensions();
    if actual_w != expected_w || actual_h != expected_h {
        return Err(format!(
            "Image dimensions differ: actual {}x{}, expected {}x{}",
            actual_w, actual_h, expected_w, expected_h
        ));
    }

    let mut mismatched = 0u64;
    let mut max_seen_diff: u8 = 0;
    for (a, e) in actual.pixels().zip(expected.pixels()) {
        let mut pixel_diff = 0u8;
        for channel in 0..4 {
            let diff = a.0[channel].abs_diff(e.0[channel]);
            pixel_diff = pixel_diff.max(diff);
        }
        max_seen_diff = max_seen_diff.max(pixel_diff);
        if pixel_diff > max_channel_diff {
            mismatched += 1;
        }
    }

    let total = actual_w as u64 * actual_h as u64;
    let fraction = mismatched as f32 / total as f32;
    println!(
        "Snapshot diff: mismatched {} / {} ({:.6}), max channel diff {}",
        mismatched, total, fraction, max_seen_diff
    );

    if fraction > max_pixel_fraction {
        return Err(format!(
            "Too many mismatched pixels: {:.6} > {:.6}",
            fraction, max_pixel_fraction
        ));
    }
    Ok(())
}
