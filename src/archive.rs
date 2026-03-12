use std::{
    collections::HashSet,
    fs,
    io::{self, Read},
    path::{self, Path},
};

use crate::core::{DirEntry, EntryLocation, format_mode, format_size};

const ARCHIVE_READ_BUFFER: usize = 1024 * 1024;

pub trait ContainerPlugin: Sync {
    fn kind(&self) -> ContainerKind;
    fn scheme(&self) -> &'static str;
    fn matches_path(&self, path: &Path) -> bool;
    fn read_dir(&self, archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>>;
    fn read_bytes_prefix(
        &self,
        archive_path: &Path,
        inner_path: &str,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>>;
    fn read_metadata(
        &self,
        archive_path: &Path,
        inner_path: &str,
    ) -> anyhow::Result<Option<(u64, Option<u32>)>>;
}

struct ZipPlugin;
struct TarPlugin;
struct TarGzPlugin;
struct TarBz2Plugin;

static ZIP_PLUGIN: ZipPlugin = ZipPlugin;
static TAR_PLUGIN: TarPlugin = TarPlugin;
static TAR_GZ_PLUGIN: TarGzPlugin = TarGzPlugin;
static TAR_BZ2_PLUGIN: TarBz2Plugin = TarBz2Plugin;

fn container_plugins() -> &'static [&'static dyn ContainerPlugin] {
    static PLUGINS: [&dyn ContainerPlugin; 4] =
        [&ZIP_PLUGIN, &TAR_PLUGIN, &TAR_GZ_PLUGIN, &TAR_BZ2_PLUGIN];
    &PLUGINS
}

fn plugin_for_kind(kind: ContainerKind) -> &'static dyn ContainerPlugin {
    for plugin in container_plugins() {
        if plugin.kind() == kind {
            return *plugin;
        }
    }
    &ZIP_PLUGIN
}

pub fn container_display_path(
    kind: ContainerKind,
    archive_path: &Path,
    inner_path: &str,
) -> String {
    let _ = kind;
    if inner_path.is_empty() {
        archive_path.to_string_lossy().to_string()
    } else {
        format!("{}/{}", archive_path.to_string_lossy(), inner_path)
    }
}

pub fn container_kind_from_path(path: &Path) -> Option<ContainerKind> {
    container_plugins()
        .iter()
        .find(|plugin| plugin.matches_path(path))
        .map(|plugin| plugin.kind())
}

pub fn is_container_path(p: &Path) -> bool {
    container_kind_from_path(p).is_some()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContainerKind {
    Zip,
    Tar,
    TarGz,
    TarBz2,
}

pub fn copy_container_entry(
    kind: ContainerKind,
    archive_path: &Path,
    inner_path: &str,
    dst_dir: &Path,
    display_name: &str,
) -> io::Result<()> {
    match kind {
        ContainerKind::Zip => copy_zip_entry(archive_path, inner_path, dst_dir, display_name),
        ContainerKind::Tar => copy_tar_entry_plain(archive_path, inner_path, dst_dir, display_name),
        ContainerKind::TarGz => copy_tar_entry_gz(archive_path, inner_path, dst_dir, display_name),
        ContainerKind::TarBz2 => {
            copy_tar_entry_bz2(archive_path, inner_path, dst_dir, display_name)
        }
    }
}

pub fn copy_container_dir(
    kind: ContainerKind,
    archive_path: &Path,
    inner_path: &str,
    dst_dir: &Path,
    display_name: &str,
) -> io::Result<()> {
    let root = dst_dir.join(display_name);
    fs::create_dir_all(&root)?;
    match kind {
        ContainerKind::Zip => copy_zip_dir(archive_path, inner_path, &root),
        ContainerKind::Tar => copy_tar_dir_plain(archive_path, inner_path, &root),
        ContainerKind::TarGz => copy_tar_dir_gz(archive_path, inner_path, &root),
        ContainerKind::TarBz2 => copy_tar_dir_bz2(archive_path, inner_path, &root),
    }
}

fn safe_rel_path(rel: &str) -> Option<path::PathBuf> {
    let candidate = path::Path::new(rel);
    let mut out = path::PathBuf::new();
    for comp in candidate.components() {
        match comp {
            path::Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn copy_zip_entry(
    archive_path: &Path,
    inner_path: &str,
    dst_dir: &Path,
    display_name: &str,
) -> io::Result<()> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let mut zip = zip::ZipArchive::new(reader).map_err(io::Error::other)?;
    let normalized = inner_path.trim_start_matches('/');
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(io::Error::other)?;
        if entry.name() == normalized {
            let target = dst_dir.join(display_name);
            if entry.is_dir() {
                fs::create_dir_all(&target)?;
                return Ok(());
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(target)?;
            io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("Entry not found in zip: {}", inner_path),
    ))
}

fn copy_zip_dir(archive_path: &Path, inner_path: &str, dst_root: &Path) -> io::Result<()> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let mut zip = zip::ZipArchive::new(reader).map_err(io::Error::other)?;
    let normalized = inner_path.trim_start_matches('/');
    let prefix = if normalized.is_empty() {
        String::new()
    } else {
        format!("{}/", normalized.trim_end_matches('/'))
    };

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(io::Error::other)?;
        let name = entry.name();
        if !name.starts_with(&prefix) {
            continue;
        }
        let rel = &name[prefix.len()..];
        let Some(rel_path) = safe_rel_path(rel) else {
            continue;
        };
        let target = dst_root.join(rel_path);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(target)?;
        io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

fn copy_tar_entry_gz(
    archive_path: &Path,
    inner_path: &str,
    dst_dir: &Path,
    display_name: &str,
) -> io::Result<()> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let decoder = flate2::read::GzDecoder::new(reader);
    copy_tar_entry(decoder, inner_path, dst_dir, display_name)
}

fn copy_tar_entry_plain(
    archive_path: &Path,
    inner_path: &str,
    dst_dir: &Path,
    display_name: &str,
) -> io::Result<()> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    copy_tar_entry(reader, inner_path, dst_dir, display_name)
}

fn copy_tar_entry_bz2(
    archive_path: &Path,
    inner_path: &str,
    dst_dir: &Path,
    display_name: &str,
) -> io::Result<()> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let decoder = bzip2::read::BzDecoder::new(reader);
    copy_tar_entry(decoder, inner_path, dst_dir, display_name)
}

fn copy_tar_entry<R: Read>(
    reader: R,
    inner_path: &str,
    dst_dir: &Path,
    display_name: &str,
) -> io::Result<()> {
    let mut archive = tar::Archive::new(reader);
    let normalized = inner_path.trim_start_matches('/');
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let name = normalize_archive_path(&path);
        if name == normalized {
            let target = dst_dir.join(display_name);
            if entry.header().entry_type().is_dir() {
                fs::create_dir_all(&target)?;
                return Ok(());
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(target)?;
            io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("Entry not found in tar: {}", inner_path),
    ))
}

fn copy_tar_dir_gz(archive_path: &Path, inner_path: &str, dst_root: &Path) -> io::Result<()> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let decoder = flate2::read::GzDecoder::new(reader);
    copy_tar_dir(decoder, inner_path, dst_root)
}

fn copy_tar_dir_plain(archive_path: &Path, inner_path: &str, dst_root: &Path) -> io::Result<()> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    copy_tar_dir(reader, inner_path, dst_root)
}

fn copy_tar_dir_bz2(archive_path: &Path, inner_path: &str, dst_root: &Path) -> io::Result<()> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let decoder = bzip2::read::BzDecoder::new(reader);
    copy_tar_dir(decoder, inner_path, dst_root)
}

fn copy_tar_dir<R: Read>(reader: R, inner_path: &str, dst_root: &Path) -> io::Result<()> {
    let mut archive = tar::Archive::new(reader);
    let normalized = inner_path.trim_start_matches('/');
    let prefix = if normalized.is_empty() {
        String::new()
    } else {
        format!("{}/", normalized.trim_end_matches('/'))
    };
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let name = normalize_archive_path(&path);
        if !name.starts_with(&prefix) {
            continue;
        }
        let rel = &name[prefix.len()..];
        let Some(rel_path) = safe_rel_path(rel) else {
            continue;
        };
        let target = dst_root.join(rel_path);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(target)?;
        io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

fn read_zip_directory(archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let mut zip = zip::ZipArchive::new(reader)?;
    let mut dirs: Vec<String> = Vec::new();
    let mut seen_dirs: HashSet<String> = HashSet::new();
    let mut files: Vec<String> = Vec::new();
    let mut seen_files: HashSet<String> = HashSet::new();

    let prefix = if cwd.is_empty() {
        "".to_string()
    } else {
        format!("{}/", cwd.trim_end_matches('/'))
    };

    for i in 0..zip.len() {
        let name = zip.by_index(i)?.name().to_string();
        if name.is_empty() || !name.starts_with(&prefix) {
            continue;
        }
        let rem = &name[prefix.len()..];
        if rem.is_empty() {
            continue;
        }
        if let Some(slash) = rem.find('/') {
            let dir = rem[..slash].to_string();
            if seen_dirs.insert(dir.clone()) {
                dirs.push(dir);
            }
        } else if seen_files.insert(rem.to_string()) {
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
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::Zip,
                archive_path: archive_path.to_path_buf(),
                inner_path: parent,
            },
            size: None,
            modified: None,
        });
    } else {
        let parent = archive_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        entries.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            is_symlink: false,
            location: EntryLocation::Fs(parent),
            size: None,
            modified: None,
        });
    }

    let dir_entries: Vec<DirEntry> = dirs
        .into_iter()
        .map(|d| DirEntry {
            name: d.clone(),
            is_dir: true,
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::Zip,
                archive_path: archive_path.to_path_buf(),
                inner_path: if cwd.is_empty() {
                    d
                } else {
                    format!("{}/{}", cwd.trim_end_matches('/'), d)
                },
            },
            size: None,
            modified: None,
        })
        .collect();

    let file_entries: Vec<DirEntry> = files
        .into_iter()
        .map(|f| DirEntry {
            name: f.clone(),
            is_dir: false,
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::Zip,
                archive_path: archive_path.to_path_buf(),
                inner_path: if cwd.is_empty() {
                    f
                } else {
                    format!("{}/{}", cwd.trim_end_matches('/'), f)
                },
            },
            size: None,
            modified: None,
        })
        .collect();
    entries.extend(dir_entries);
    entries.extend(file_entries);

    Ok(entries)
}

pub fn format_container_listing(
    kind: ContainerKind,
    archive_path: &Path,
    entries: &[DirEntry],
    max_entries: usize,
) -> String {
    let mut out = String::new();
    out.push_str("Contents:\n");
    let mut count = 0usize;
    for entry in entries.iter() {
        if entry.name == ".." {
            continue;
        }
        if count >= max_entries {
            out.push_str(&format!(
                "… and {} more\n",
                entries.len().saturating_sub(count)
            ));
            break;
        }
        let size = read_container_metadata(kind, archive_path, entry_name_for_metadata(entry))
            .ok()
            .flatten()
            .map(|(size, mode)| (format_size(size), mode));
        let mode_str = size.as_ref().and_then(|pair| pair.1.map(format_mode));
        let size_str = size.as_ref().map(|pair| pair.0.as_str());

        let mut line = String::new();
        if let Some(mode) = mode_str {
            line.push_str(&mode);
            line.push(' ');
        } else {
            line.push_str("---- ");
        }
        if let Some(size) = size_str {
            line.push_str(size);
        } else {
            line.push_str("    -");
        }
        line.push(' ');
        line.push_str(&entry_display_name(entry));
        line.push('\n');
        out.push_str(&line);
        count += 1;
    }
    out
}

fn entry_display_name(entry: &DirEntry) -> String {
    if entry.is_dir {
        format!("{}/", entry.name)
    } else {
        entry.name.clone()
    }
}

fn entry_name_for_metadata(entry: &DirEntry) -> &str {
    if entry.is_dir {
        entry.name.trim_end_matches('/')
    } else {
        entry.name.as_str()
    }
}

fn read_zip_bytes_prefix(
    archive_path: &Path,
    inner_path: &str,
    max_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let mut zip = zip::ZipArchive::new(reader)?;
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

fn read_tar_directory(archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let mut archive = tar::Archive::new(reader);
    let mut dirs: Vec<String> = Vec::new();
    let mut seen_dirs: HashSet<String> = HashSet::new();
    let mut files: Vec<String> = Vec::new();
    let mut seen_files: HashSet<String> = HashSet::new();

    let prefix = if cwd.is_empty() {
        "".to_string()
    } else {
        format!("{}/", cwd.trim_end_matches('/'))
    };

    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        let name = normalize_archive_path(&path);
        if name.is_empty() || !name.starts_with(&prefix) {
            continue;
        }
        let rem = &name[prefix.len()..];
        if rem.is_empty() {
            continue;
        }
        if let Some(slash) = rem.find('/') {
            let dir = rem[..slash].to_string();
            if seen_dirs.insert(dir.clone()) {
                dirs.push(dir);
            }
        } else if seen_files.insert(rem.to_string()) {
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
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::Tar,
                archive_path: archive_path.to_path_buf(),
                inner_path: parent,
            },
            size: None,
            modified: None,
        });
    } else {
        let parent = archive_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        entries.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            is_symlink: false,
            location: EntryLocation::Fs(parent),
            size: None,
            modified: None,
        });
    }

    let dir_entries: Vec<DirEntry> = dirs
        .into_iter()
        .map(|d| DirEntry {
            name: d.clone(),
            is_dir: true,
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::Tar,
                archive_path: archive_path.to_path_buf(),
                inner_path: if cwd.is_empty() {
                    d
                } else {
                    format!("{}/{}", cwd.trim_end_matches('/'), d)
                },
            },
            size: None,
            modified: None,
        })
        .collect();

    let file_entries: Vec<DirEntry> = files
        .into_iter()
        .map(|f| DirEntry {
            name: f.clone(),
            is_dir: false,
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::Tar,
                archive_path: archive_path.to_path_buf(),
                inner_path: if cwd.is_empty() {
                    f
                } else {
                    format!("{}/{}", cwd.trim_end_matches('/'), f)
                },
            },
            size: None,
            modified: None,
        })
        .collect();
    entries.extend(dir_entries);
    entries.extend(file_entries);

    Ok(entries)
}

fn read_tar_gz_directory(archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let decoder = flate2::read::GzDecoder::new(reader);
    let mut archive = tar::Archive::new(decoder);
    let mut dirs: Vec<String> = Vec::new();
    let mut seen_dirs: HashSet<String> = HashSet::new();
    let mut files: Vec<String> = Vec::new();
    let mut seen_files: HashSet<String> = HashSet::new();

    let prefix = if cwd.is_empty() {
        "".to_string()
    } else {
        format!("{}/", cwd.trim_end_matches('/'))
    };

    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        let name = normalize_archive_path(&path);
        if name.is_empty() || !name.starts_with(&prefix) {
            continue;
        }
        let rem = &name[prefix.len()..];
        if rem.is_empty() {
            continue;
        }
        if let Some(slash) = rem.find('/') {
            let dir = rem[..slash].to_string();
            if seen_dirs.insert(dir.clone()) {
                dirs.push(dir);
            }
        } else if seen_files.insert(rem.to_string()) {
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
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::TarGz,
                archive_path: archive_path.to_path_buf(),
                inner_path: parent,
            },
            size: None,
            modified: None,
        });
    } else {
        let parent = archive_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        entries.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            is_symlink: false,
            location: EntryLocation::Fs(parent),
            size: None,
            modified: None,
        });
    }

    let dir_entries: Vec<DirEntry> = dirs
        .into_iter()
        .map(|d| DirEntry {
            name: d.clone(),
            is_dir: true,
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::TarGz,
                archive_path: archive_path.to_path_buf(),
                inner_path: if cwd.is_empty() {
                    d
                } else {
                    format!("{}/{}", cwd.trim_end_matches('/'), d)
                },
            },
            size: None,
            modified: None,
        })
        .collect();

    let file_entries: Vec<DirEntry> = files
        .into_iter()
        .map(|f| DirEntry {
            name: f.clone(),
            is_dir: false,
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::TarGz,
                archive_path: archive_path.to_path_buf(),
                inner_path: if cwd.is_empty() {
                    f
                } else {
                    format!("{}/{}", cwd.trim_end_matches('/'), f)
                },
            },
            size: None,
            modified: None,
        })
        .collect();
    entries.extend(dir_entries);
    entries.extend(file_entries);

    Ok(entries)
}

fn read_tar_bytes_prefix(
    archive_path: &Path,
    inner_path: &str,
    max_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let mut archive = tar::Archive::new(reader);
    let normalized = inner_path.trim_start_matches('/');
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let name = normalize_archive_path(&path);
        if name == normalized {
            let mut data = Vec::new();
            entry
                .by_ref()
                .take(max_bytes as u64)
                .read_to_end(&mut data)?;
            return Ok(data);
        }
    }
    Err(anyhow::anyhow!(format!(
        "Entry not found in tar: {}",
        inner_path
    )))
}

fn read_tar_gz_bytes_prefix(
    archive_path: &Path,
    inner_path: &str,
    max_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let decoder = flate2::read::GzDecoder::new(reader);
    let mut archive = tar::Archive::new(decoder);
    let normalized = inner_path.trim_start_matches('/');
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let name = normalize_archive_path(&path);
        if name == normalized {
            let mut data = Vec::new();
            entry
                .by_ref()
                .take(max_bytes as u64)
                .read_to_end(&mut data)?;
            return Ok(data);
        }
    }
    Err(anyhow::anyhow!(format!(
        "Entry not found in tar.gz: {}",
        inner_path
    )))
}

pub fn normalize_archive_path(path: &Path) -> String {
    use std::path::Component;
    let mut parts: Vec<String> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => {
                let s = seg.to_string_lossy();
                if !s.is_empty() {
                    parts.push(s.into_owned());
                }
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                parts.pop();
            }
        }
    }
    parts.join("/")
}

fn read_tar_bz2_directory_with_progress(
    archive_path: &Path,
    cwd: &str,
    progress: &mut dyn FnMut(usize),
) -> anyhow::Result<Vec<DirEntry>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let decoder = bzip2::read::BzDecoder::new(reader);
    let mut archive = tar::Archive::new(decoder);
    let mut dirs: Vec<String> = Vec::new();
    let mut seen_dirs: HashSet<String> = HashSet::new();
    let mut files: Vec<String> = Vec::new();
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut seen = 0usize;
    const PROGRESS_INTERVAL: usize = 1000;

    let prefix = if cwd.is_empty() {
        "".to_string()
    } else {
        format!("{}/", cwd.trim_end_matches('/'))
    };

    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        let name = normalize_archive_path(&path);
        if name.is_empty() || !name.starts_with(&prefix) {
            seen += 1;
            if seen.is_multiple_of(PROGRESS_INTERVAL) {
                progress(seen);
            }
            continue;
        }
        let rem = &name[prefix.len()..];
        if rem.is_empty() {
            seen += 1;
            if seen.is_multiple_of(PROGRESS_INTERVAL) {
                progress(seen);
            }
            continue;
        }
        if let Some(slash) = rem.find('/') {
            let dir = rem[..slash].to_string();
            if seen_dirs.insert(dir.clone()) {
                dirs.push(dir);
            }
        } else if seen_files.insert(rem.to_string()) {
            files.push(rem.to_string());
        }
        seen += 1;
        if seen.is_multiple_of(PROGRESS_INTERVAL) {
            progress(seen);
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
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::TarBz2,
                archive_path: archive_path.to_path_buf(),
                inner_path: parent,
            },
            size: None,
            modified: None,
        });
    } else {
        let parent = archive_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        entries.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            is_symlink: false,
            location: EntryLocation::Fs(parent),
            size: None,
            modified: None,
        });
    }

    let dir_entries: Vec<DirEntry> = dirs
        .into_iter()
        .map(|d| DirEntry {
            name: d.clone(),
            is_dir: true,
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::TarBz2,
                archive_path: archive_path.to_path_buf(),
                inner_path: if cwd.is_empty() {
                    d
                } else {
                    format!("{}/{}", cwd.trim_end_matches('/'), d)
                },
            },
            size: None,
            modified: None,
        })
        .collect();

    let file_entries: Vec<DirEntry> = files
        .into_iter()
        .map(|f| DirEntry {
            name: f.clone(),
            is_dir: false,
            is_symlink: false,
            location: EntryLocation::Container {
                kind: ContainerKind::TarBz2,
                archive_path: archive_path.to_path_buf(),
                inner_path: if cwd.is_empty() {
                    f
                } else {
                    format!("{}/{}", cwd.trim_end_matches('/'), f)
                },
            },
            size: None,
            modified: None,
        })
        .collect();
    entries.extend(dir_entries);
    entries.extend(file_entries);

    Ok(entries)
}

fn read_tar_bz2_directory(archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
    read_tar_bz2_directory_with_progress(archive_path, cwd, &mut |_| {})
}

fn read_tar_bz2_bytes_prefix(
    archive_path: &Path,
    inner_path: &str,
    max_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
    let decoder = bzip2::read::BzDecoder::new(reader);
    let mut archive = tar::Archive::new(decoder);
    let normalized = inner_path.trim_start_matches('/');
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let name = normalize_archive_path(&path);
        if name == normalized {
            let mut data = Vec::new();
            entry
                .by_ref()
                .take(max_bytes as u64)
                .read_to_end(&mut data)?;
            return Ok(data);
        }
    }
    Err(anyhow::anyhow!(format!(
        "Entry not found in tar.bz2: {}",
        inner_path
    )))
}

pub fn read_container_directory(
    kind: ContainerKind,
    archive_path: &Path,
    cwd: &str,
) -> anyhow::Result<Vec<DirEntry>> {
    plugin_for_kind(kind).read_dir(archive_path, cwd)
}

pub fn read_container_directory_with_progress(
    kind: ContainerKind,
    archive_path: &Path,
    cwd: &str,
    mut progress: impl FnMut(usize),
) -> anyhow::Result<Vec<DirEntry>> {
    match kind {
        ContainerKind::TarBz2 => {
            read_tar_bz2_directory_with_progress(archive_path, cwd, &mut progress)
        }
        _ => {
            let entries = read_container_directory(kind, archive_path, cwd)?;
            progress(entries.len());
            Ok(entries)
        }
    }
}

pub fn read_container_bytes_prefix(
    kind: ContainerKind,
    archive_path: &Path,
    inner_path: &str,
    max_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    plugin_for_kind(kind).read_bytes_prefix(archive_path, inner_path, max_bytes)
}

pub fn read_container_metadata(
    kind: ContainerKind,
    archive_path: &Path,
    inner_path: &str,
) -> anyhow::Result<Option<(u64, Option<u32>)>> {
    plugin_for_kind(kind).read_metadata(archive_path, inner_path)
}

impl ContainerPlugin for ZipPlugin {
    fn kind(&self) -> ContainerKind {
        ContainerKind::Zip
    }

    fn scheme(&self) -> &'static str {
        "zip"
    }

    fn matches_path(&self, path: &Path) -> bool {
        matches!(
            path.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase()),
            Some(ext) if ext == "zip"
        )
    }

    fn read_dir(&self, archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
        read_zip_directory(archive_path, cwd)
    }

    fn read_bytes_prefix(
        &self,
        archive_path: &Path,
        inner_path: &str,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        read_zip_bytes_prefix(archive_path, inner_path, max_bytes)
    }

    fn read_metadata(
        &self,
        archive_path: &Path,
        inner_path: &str,
    ) -> anyhow::Result<Option<(u64, Option<u32>)>> {
        let file = fs::File::open(archive_path)?;
        let mut zip = zip::ZipArchive::new(file)?;
        let normalized = inner_path.trim_start_matches('/');
        for i in 0..zip.len() {
            let entry = zip.by_index(i)?;
            if entry.name() == normalized {
                let size = entry.size();
                let mode = entry.unix_mode();
                return Ok(Some((size, mode)));
            }
        }
        Ok(None)
    }
}

impl ContainerPlugin for TarPlugin {
    fn kind(&self) -> ContainerKind {
        ContainerKind::Tar
    }

    fn scheme(&self) -> &'static str {
        "tar"
    }

    fn matches_path(&self, path: &Path) -> bool {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        name.ends_with(".tar")
    }

    fn read_dir(&self, archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
        read_tar_directory(archive_path, cwd)
    }

    fn read_bytes_prefix(
        &self,
        archive_path: &Path,
        inner_path: &str,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        read_tar_bytes_prefix(archive_path, inner_path, max_bytes)
    }

    fn read_metadata(
        &self,
        archive_path: &Path,
        inner_path: &str,
    ) -> anyhow::Result<Option<(u64, Option<u32>)>> {
        let file = fs::File::open(archive_path)?;
        let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
        let mut archive = tar::Archive::new(reader);
        let normalized = inner_path.trim_start_matches('/');
        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?;
            let name = normalize_archive_path(&path);
            if name == normalized {
                let size = entry.size();
                let mode = entry.header().mode().ok();
                return Ok(Some((size, mode)));
            }
        }
        Ok(None)
    }
}

impl ContainerPlugin for TarGzPlugin {
    fn kind(&self) -> ContainerKind {
        ContainerKind::TarGz
    }

    fn scheme(&self) -> &'static str {
        "tar.gz"
    }

    fn matches_path(&self, path: &Path) -> bool {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        name.ends_with(".tar.gz") || name.ends_with(".tgz")
    }

    fn read_dir(&self, archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
        read_tar_gz_directory(archive_path, cwd)
    }

    fn read_bytes_prefix(
        &self,
        archive_path: &Path,
        inner_path: &str,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        read_tar_gz_bytes_prefix(archive_path, inner_path, max_bytes)
    }

    fn read_metadata(
        &self,
        archive_path: &Path,
        inner_path: &str,
    ) -> anyhow::Result<Option<(u64, Option<u32>)>> {
        let file = fs::File::open(archive_path)?;
        let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
        let decoder = flate2::read::GzDecoder::new(reader);
        let mut archive = tar::Archive::new(decoder);
        let normalized = inner_path.trim_start_matches('/');
        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?;
            let name = normalize_archive_path(&path);
            if name == normalized {
                let size = entry.size();
                let mode = entry.header().mode().ok();
                return Ok(Some((size, mode)));
            }
        }
        Ok(None)
    }
}

impl ContainerPlugin for TarBz2Plugin {
    fn kind(&self) -> ContainerKind {
        ContainerKind::TarBz2
    }

    fn scheme(&self) -> &'static str {
        "tar.bz2"
    }

    fn matches_path(&self, path: &Path) -> bool {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        name.ends_with(".tar.bz2") || name.ends_with(".tbz") || name.ends_with(".tbz2")
    }

    fn read_dir(&self, archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
        read_tar_bz2_directory(archive_path, cwd)
    }

    fn read_bytes_prefix(
        &self,
        archive_path: &Path,
        inner_path: &str,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        read_tar_bz2_bytes_prefix(archive_path, inner_path, max_bytes)
    }

    fn read_metadata(
        &self,
        archive_path: &Path,
        inner_path: &str,
    ) -> anyhow::Result<Option<(u64, Option<u32>)>> {
        let file = fs::File::open(archive_path)?;
        let reader = std::io::BufReader::with_capacity(ARCHIVE_READ_BUFFER, file);
        let decoder = bzip2::read::BzDecoder::new(reader);
        let mut archive = tar::Archive::new(decoder);
        let normalized = inner_path.trim_start_matches('/');
        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?;
            let name = normalize_archive_path(&path);
            if name == normalized {
                let size = entry.size();
                let mode = entry.header().mode().ok();
                return Ok(Some((size, mode)));
            }
        }
        Ok(None)
    }
}
