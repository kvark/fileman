use std::env;
use std::path::PathBuf;

use fileman::snapshot::compare_snapshots;

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
    let diff = compare_snapshots(
        &actual_path,
        &expected_path,
        max_channel_diff,
        max_pixel_fraction,
    )?;
    println!(
        "Snapshot diff: mismatched {} / {} ({:.6}), max channel diff {}",
        diff.mismatched, diff.total, diff.fraction, diff.max_channel_diff
    );
    Ok(())
}
