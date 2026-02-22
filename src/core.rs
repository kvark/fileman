use std::{
    collections::HashSet,
    fs,
    io::{self, Read},
    path::{self, Path},
    sync::Arc,
};

#[derive(Clone)]
pub enum EntryLocation {
    Fs(path::PathBuf),
    Zip {
        archive_path: path::PathBuf,
        inner_path: String, // no leading slash, '' means root
    },
}

#[derive(Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub location: EntryLocation,
}

pub enum DirBatch {
    Append(Vec<DirEntry>),
    Replace(Vec<DirEntry>),
}

#[derive(Clone, PartialEq, Debug)]
pub enum ActivePanel {
    Left,
    Right,
}

pub enum PanelMode {
    Fs,
    Zip {
        archive_path: path::PathBuf,
        cwd: String,
    },
}

pub enum PreviewContent {
    Text(String),
    Image(Arc<Path>),
}

pub enum PreviewRequest {
    Read {
        id: u64,
        location: EntryLocation,
        max_bytes: usize,
    },
}

pub enum IOTask {
    Copy {
        src: path::PathBuf,
        dst_dir: path::PathBuf,
    },
}

pub fn copy_recursively(src: &Path, dst_dir: &Path) -> io::Result<()> {
    if src.is_dir() {
        let dest = dst_dir.join(src.file_name().unwrap());
        fs::create_dir_all(&dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                copy_recursively(&path, &dest)?;
            } else {
                fs::copy(&path, dest.join(entry.file_name()))?;
            }
        }
    } else {
        let dest = dst_dir.join(src.file_name().unwrap());
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dest)?;
    }
    Ok(())
}

pub fn read_fs_directory(path: &path::Path) -> anyhow::Result<Vec<DirEntry>> {
    let mut entries = Vec::new();

    let read_dir = fs::read_dir(path)?;
    let mut dir_entries = Vec::new();

    for entry in read_dir {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy().to_string();

        let file_type = entry.file_type()?;
        let is_dir = file_type.is_dir();

        dir_entries.push(DirEntry {
            name: file_name,
            is_dir,
            location: EntryLocation::Fs(entry.path()),
        });
    }

    dir_entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

    if path.parent().is_some() {
        entries.push(DirEntry {
            name: "..".to_string(),
            is_dir: true,
            location: EntryLocation::Fs(path.parent().unwrap().to_path_buf()),
        });
    }

    entries.extend(dir_entries);

    Ok(entries)
}

pub fn read_zip_directory(archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
    let file = fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut dirs: HashSet<String> = HashSet::new();
    let mut files: Vec<String> = Vec::new();

    let prefix = if cwd.is_empty() {
        "".to_string()
    } else {
        format!("{}/", cwd.trim_end_matches('/'))
    };

    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        let name = entry.name();
        if !name.starts_with(&prefix) {
            continue;
        }
        let rem = &name[prefix.len()..];
        if rem.is_empty() {
            continue;
        }
        if let Some(slash) = rem.find('/') {
            let dir = rem[..slash].to_string();
            dirs.insert(dir);
        } else {
            files.push(rem.to_string());
        }
    }

    let mut entries: Vec<DirEntry> = Vec::new();

    if !cwd.is_empty() {
        let parent = cwd
            .trim_end_matches('/')
            .rsplit_once('/')
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
        entries.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            location: EntryLocation::Zip {
                archive_path: archive_path.to_path_buf(),
                inner_path: parent,
            },
        });
    } else if let Some(parent) = archive_path.parent() {
        entries.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            location: EntryLocation::Fs(parent.to_path_buf()),
        });
    }

    let mut dir_entries: Vec<DirEntry> = dirs
        .into_iter()
        .map(|d| DirEntry {
            name: d.clone(),
            is_dir: true,
            location: EntryLocation::Zip {
                archive_path: archive_path.to_path_buf(),
                inner_path: if cwd.is_empty() {
                    d
                } else {
                    format!("{}/{}", cwd.trim_end_matches('/'), d)
                },
            },
        })
        .collect();

    let mut file_entries: Vec<DirEntry> = files
        .into_iter()
        .map(|f| DirEntry {
            name: f.clone(),
            is_dir: false,
            location: EntryLocation::Zip {
                archive_path: archive_path.to_path_buf(),
                inner_path: if cwd.is_empty() {
                    f
                } else {
                    format!("{}/{}", cwd.trim_end_matches('/'), f)
                },
            },
        })
        .collect();

    dir_entries.sort_by(|a, b| a.name.cmp(&b.name));
    file_entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries.extend(dir_entries);
    entries.extend(file_entries);

    Ok(entries)
}

pub fn is_zip_path(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase()),
        Some(ext) if ext == "zip"
    )
}

pub fn format_preview_info(kind: &str, location: &EntryLocation) -> String {
    match location {
        EntryLocation::Fs(path) => format!("{kind}\n{}", path.to_string_lossy()),
        EntryLocation::Zip {
            archive_path,
            inner_path,
        } => {
            let display = if inner_path.is_empty() {
                format!("{}::zip:/", archive_path.to_string_lossy())
            } else {
                format!("{}::zip:/{}", archive_path.to_string_lossy(), inner_path)
            };
            format!("{kind}\n{display}")
        }
    }
}

pub fn is_image_path(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase()),
        Some(ext) if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp")
    )
}

pub fn is_text_path(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase()),
        Some(ext)
            if matches!(
                ext.as_str(),
                "txt" | "md" | "json" | "toml" | "yaml" | "yml" | "rs" | "log" | "ini" | "csv"
            )
    )
}

pub fn read_text_preview(path: &Path, max_bytes: usize) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.by_ref().take(max_bytes as u64).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub fn read_bytes_prefix(path: &Path, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.by_ref().take(max_bytes as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

pub fn read_zip_bytes_prefix(
    archive_path: &Path,
    inner_path: &str,
    max_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let file = fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let normalized = inner_path.trim_start_matches('/');
    let mut data = Vec::new();
    let mut found = None;
    for i in 0..zip.len() {
        let name = zip.by_index(i)?.name().to_string();
        if name == normalized {
            found = Some(i);
            break;
        }
    }
    if let Some(idx) = found {
        let mut zf = zip.by_index(idx)?;
        zf.by_ref().take(max_bytes as u64).read_to_end(&mut data)?;
        Ok(data)
    } else {
        Err(anyhow::anyhow!(format!(
            "Entry not found in zip: {}",
            inner_path
        )))
    }
}

pub fn hexdump(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut offset = 0usize;
    for chunk in bytes.chunks(16) {
        out.push_str(&format!("{:08x}: ", offset));
        for i in 0..16 {
            if i < chunk.len() {
                out.push_str(&format!("{:02x} ", chunk[i]));
            } else {
                out.push_str("   ");
            }
            if i == 7 {
                out.push(' ');
            }
        }
        out.push(' ');
        for &b in chunk {
            let ch = if (0x20..=0x7e).contains(&b) {
                b as char
            } else {
                '.'
            };
            out.push(ch);
        }
        out.push('\n');
        offset += 16;
    }
    out
}

pub fn is_probably_text(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    if bytes.contains(&0) {
        return false;
    }
    let mut printable = 0usize;
    for &b in bytes {
        match b {
            0x09 | 0x0A | 0x0D => printable += 1,
            0x20..=0x7E => printable += 1,
            _ => {}
        }
    }
    let ratio = printable as f32 / bytes.len().max(1) as f32;
    ratio > 0.85
}
