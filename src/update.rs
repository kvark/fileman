use std::path::PathBuf;

const GITHUB_REPO: &str = "kvark/fileman";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug)]
pub struct Release {
    pub tag: String,
    pub version: semver::Version,
    pub asset_url: String,
    pub asset_name: String,
}

fn current_version() -> semver::Version {
    semver::Version::parse(CURRENT_VERSION).expect("invalid CARGO_PKG_VERSION")
}

fn asset_suffix() -> Option<&'static str> {
    if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        Some("linux-x86_64.tar.gz")
    } else if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
        Some("windows-x86_64.zip")
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        Some("macos-aarch64.zip")
    } else {
        None
    }
}

pub fn check_for_update() -> anyhow::Result<Option<Release>> {
    let suffix = asset_suffix()
        .ok_or_else(|| anyhow::anyhow!("unsupported platform for self-update"))?;

    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("fileman/{CURRENT_VERSION}"))
        .build()?;

    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let resp: serde_json::Value = client.get(&url).send()?.error_for_status()?.json()?;

    let tag = resp["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing tag_name in release response"))?;
    let version_str = tag.strip_prefix('v').unwrap_or(tag);
    let remote_version = semver::Version::parse(version_str)
        .map_err(|e| anyhow::anyhow!("invalid version '{version_str}': {e}"))?;

    if remote_version <= current_version() {
        return Ok(None);
    }

    let assets = resp["assets"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing assets in release response"))?;

    let asset = assets
        .iter()
        .find(|a| {
            a["name"]
                .as_str()
                .map_or(false, |n: &str| n.ends_with(suffix) && !n.contains("-gles"))
        })
        .ok_or_else(|| anyhow::anyhow!("no matching asset for suffix '{suffix}'"))?;

    let asset_url = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing download URL for asset"))?;
    let asset_name = asset["name"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();

    Ok(Some(Release {
        tag: tag.to_string(),
        version: remote_version,
        asset_url: asset_url.to_string(),
        asset_name,
    }))
}

pub fn perform_update(release: &Release) -> anyhow::Result<()> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("fileman/{CURRENT_VERSION}"))
        .build()?;

    eprintln!("Downloading {}...", release.asset_name);
    let bytes = client
        .get(&release.asset_url)
        .send()?
        .error_for_status()?
        .bytes()?;

    let current_exe = std::env::current_exe()?;
    extract_and_replace(&bytes, &release.asset_name, &current_exe)?;

    // On macOS, re-sign the .app bundle if we're inside one
    #[cfg(target_os = "macos")]
    if let Some(app_dir) = find_app_bundle(&current_exe) {
        let _ = std::process::Command::new("codesign")
            .args(["--sign", "-", "--deep", "--force"])
            .arg(&app_dir)
            .status();
    }

    eprintln!("Updated to {} successfully.", release.version);
    Ok(())
}

fn extract_and_replace(
    archive_bytes: &[u8],
    asset_name: &str,
    target: &std::path::Path,
) -> anyhow::Result<()> {
    let binary_name = if cfg!(windows) {
        "fileman.exe"
    } else {
        "fileman"
    };

    let new_binary = if asset_name.ends_with(".tar.gz") {
        extract_from_tar_gz(archive_bytes, binary_name)?
    } else if asset_name.ends_with(".zip") {
        extract_from_zip(archive_bytes, binary_name)?
    } else {
        anyhow::bail!("unknown archive format: {asset_name}");
    };

    atomic_replace(target, &new_binary)
}

fn extract_from_tar_gz(data: &[u8], name: &str) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    let gz = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.file_name().map_or(false, |f| f == name) {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    anyhow::bail!("binary '{name}' not found in tar.gz archive")
}

fn extract_from_zip(data: &[u8], name: &str) -> anyhow::Result<Vec<u8>> {
    use std::io::{Cursor, Read};
    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let path = PathBuf::from(file.name());
        if path.file_name().map_or(false, |f| f == name) {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    anyhow::bail!("binary '{name}' not found in zip archive")
}

fn atomic_replace(target: &std::path::Path, new_binary: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;

    let dir = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot determine parent directory of executable"))?;

    // Write to a temp file next to the target
    let tmp_path = dir.join(".fileman-update.tmp");
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(new_binary)?;
        f.sync_all()?;
    }

    // Set executable permission on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))?;
    }

    // On Windows, rename the running exe out of the way first
    #[cfg(windows)]
    {
        let old_path = dir.join(".fileman-old.exe");
        let _ = std::fs::remove_file(&old_path);
        std::fs::rename(target, &old_path)?;
    }

    std::fs::rename(&tmp_path, target)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn find_app_bundle(exe: &std::path::Path) -> Option<PathBuf> {
    // Typical path: Fileman.app/Contents/MacOS/fileman
    let contents = exe.parent()?; // MacOS
    let contents = contents.parent()?; // Contents
    let app = contents.parent()?; // Fileman.app
    if app.extension().map_or(false, |e| e == "app") {
        Some(app.to_path_buf())
    } else {
        None
    }
}
