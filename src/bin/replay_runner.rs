use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let fileman = exe.with_file_name("fileman");
    if !fileman.exists() {
        return Err(format!(
            "Expected fileman binary at {}",
            fileman.to_string_lossy()
        ));
    }

    let cases_dir = PathBuf::from("tests/cases");
    let mut cases: Vec<PathBuf> = Vec::new();
    let read = std::fs::read_dir(&cases_dir)
        .map_err(|err| format!("Failed to read {}: {err}", cases_dir.display()))?;
    for entry in read {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        if path.extension() == Some(OsStr::new("ron")) {
            cases.push(path);
        }
    }
    cases.sort();
    if cases.is_empty() {
        return Err("No replay cases found in tests/cases".to_string());
    }

    let mut failures = 0usize;
    for case in cases {
        let status = Command::new(&fileman)
            .arg("--replay")
            .arg(&case)
            .status()
            .map_err(|err| format!("Failed to run {}: {err}", case.display()))?;
        if !status.success() {
            eprintln!("Replay failed: {}", case.display());
            failures += 1;
        }
    }

    if failures > 0 {
        Err(format!("{failures} replay case(s) failed"))
    } else {
        Ok(())
    }
}
