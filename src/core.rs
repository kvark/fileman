use std::{
    collections::HashSet,
    fs,
    io::{self, Read},
    path::{self, Path},
    sync::Arc,
};

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
struct TarGzPlugin;
struct TarBz2Plugin;

static ZIP_PLUGIN: ZipPlugin = ZipPlugin;
static TAR_GZ_PLUGIN: TarGzPlugin = TarGzPlugin;
static TAR_BZ2_PLUGIN: TarBz2Plugin = TarBz2Plugin;

fn container_plugins() -> &'static [&'static dyn ContainerPlugin] {
    static PLUGINS: [&dyn ContainerPlugin; 3] = [&ZIP_PLUGIN, &TAR_GZ_PLUGIN, &TAR_BZ2_PLUGIN];
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

fn container_scheme(kind: ContainerKind) -> &'static str {
    plugin_for_kind(kind).scheme()
}

pub fn container_display_path(
    kind: ContainerKind,
    archive_path: &Path,
    inner_path: &str,
) -> String {
    if inner_path.is_empty() {
        format!(
            "{}::{}:/",
            archive_path.to_string_lossy(),
            container_scheme(kind)
        )
    } else {
        format!(
            "{}::{}:/{}",
            archive_path.to_string_lossy(),
            container_scheme(kind),
            inner_path
        )
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
    TarGz,
    TarBz2,
}

#[derive(Clone)]
pub enum EntryLocation {
    Fs(path::PathBuf),
    Container {
        kind: ContainerKind,
        archive_path: path::PathBuf,
        inner_path: String, // no leading slash, '' means root
    },
}

impl EntryLocation {
    pub fn display_name(&self) -> String {
        match self {
            EntryLocation::Fs(path) => path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("<unknown>")
                .to_string(),
            EntryLocation::Container { inner_path, .. } => inner_path
                .rsplit('/')
                .next()
                .unwrap_or("<unknown>")
                .to_string(),
        }
    }
}

#[derive(Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub location: EntryLocation,
    pub size: Option<u64>,
}

pub enum DirBatch {
    Append(Vec<DirEntry>),
    Replace(Vec<DirEntry>),
    Loading,
    Progress { loaded: usize, total: Option<usize> },
    Error(String),
}

#[derive(Clone, PartialEq, Debug)]
pub enum ActivePanel {
    Left,
    Right,
}

#[derive(Clone)]
pub enum PanelMode {
    Fs,
    Container {
        kind: ContainerKind,
        archive_path: path::PathBuf,
        cwd: String,
    },
    Search {
        root: path::PathBuf,
        query: String,
        mode: SearchMode,
        case: SearchCase,
    },
}

pub enum PreviewContent {
    Text(String),
    Image(ImageLocation),
}

#[derive(Clone)]
pub enum ImageLocation {
    Fs(Arc<Path>),
    Container {
        kind: ContainerKind,
        archive_path: path::PathBuf,
        inner_path: String,
    },
}

pub enum PreviewRequest {
    Read {
        id: u64,
        location: EntryLocation,
        max_bytes: usize,
    },
    ListContainer {
        id: u64,
        kind: ContainerKind,
        archive_path: path::PathBuf,
        max_entries: usize,
    },
}

pub enum IOTask {
    Copy {
        src: path::PathBuf,
        dst_dir: path::PathBuf,
    },
    CopyContainer {
        kind: ContainerKind,
        archive_path: path::PathBuf,
        inner_path: String,
        dst_dir: path::PathBuf,
        display_name: String,
    },
    CopyContainerDir {
        kind: ContainerKind,
        archive_path: path::PathBuf,
        inner_path: String,
        dst_dir: path::PathBuf,
        display_name: String,
    },
    Move {
        src: path::PathBuf,
        dst_dir: path::PathBuf,
    },
    Delete {
        target: path::PathBuf,
    },
    Rename {
        src: path::PathBuf,
        new_name: String,
    },
}

pub enum IOResult {
    Completed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Name,
    Content,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchCase {
    Sensitive,
    Insensitive,
}

pub struct SearchRequest {
    pub id: u64,
    pub root: path::PathBuf,
    pub needle: String,
    pub case: SearchCase,
    pub mode: SearchMode,
}

#[derive(Clone)]
pub struct SearchResult {
    pub path: path::PathBuf,
    pub is_dir: bool,
    pub size: Option<u64>,
}

#[derive(Clone, Copy)]
pub struct SearchProgress {
    pub scanned: usize,
    pub matched: usize,
}

pub enum SearchEvent {
    Match { id: u64, result: SearchResult },
    Progress { id: u64, progress: SearchProgress },
    Done { id: u64, progress: SearchProgress },
    Error { id: u64, message: String },
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

pub fn copy_container_entry(
    kind: ContainerKind,
    archive_path: &Path,
    inner_path: &str,
    dst_dir: &Path,
    display_name: &str,
) -> io::Result<()> {
    match kind {
        ContainerKind::Zip => copy_zip_entry(archive_path, inner_path, dst_dir, display_name),
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
    let reader = std::io::BufReader::new(file);
    let mut zip =
        zip::ZipArchive::new(reader).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let normalized = inner_path.trim_start_matches('/');
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
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
    let reader = std::io::BufReader::new(file);
    let mut zip =
        zip::ZipArchive::new(reader).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let normalized = inner_path.trim_matches('/');
    let prefix = if normalized.is_empty() {
        "".to_string()
    } else {
        format!("{}/", normalized)
    };
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let name = entry.name().to_string();
        if !name.starts_with(&prefix) {
            continue;
        }
        let rel = &name[prefix.len()..];
        if rel.is_empty() {
            continue;
        }
        let rel_path = match safe_rel_path(rel.trim_end_matches('/')) {
            Some(path) => path,
            None => continue,
        };
        let target = dst_root.join(rel_path);
        if entry.is_dir() || name.ends_with('/') {
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
    let reader = std::io::BufReader::new(file);
    let decoder = flate2::read::GzDecoder::new(reader);
    copy_tar_entry(decoder, inner_path, dst_dir, display_name)
}

fn copy_tar_entry_bz2(
    archive_path: &Path,
    inner_path: &str,
    dst_dir: &Path,
    display_name: &str,
) -> io::Result<()> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::new(file);
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
    let reader = std::io::BufReader::new(file);
    let decoder = flate2::read::GzDecoder::new(reader);
    copy_tar_dir(decoder, inner_path, dst_root)
}

fn copy_tar_dir_bz2(archive_path: &Path, inner_path: &str, dst_root: &Path) -> io::Result<()> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::new(file);
    let decoder = bzip2::read::BzDecoder::new(reader);
    copy_tar_dir(decoder, inner_path, dst_root)
}

fn copy_tar_dir<R: Read>(reader: R, inner_path: &str, dst_root: &Path) -> io::Result<()> {
    let mut archive = tar::Archive::new(reader);
    let normalized = inner_path.trim_matches('/');
    let prefix = if normalized.is_empty() {
        "".to_string()
    } else {
        format!("{}/", normalized)
    };
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let name = normalize_archive_path(&path);
        if !name.starts_with(&prefix) {
            continue;
        }
        let rel = &name[prefix.len()..];
        if rel.is_empty() {
            continue;
        }
        let rel_path = match safe_rel_path(rel) {
            Some(path) => path,
            None => continue,
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

        let size = if is_dir {
            None
        } else {
            entry.metadata().ok().map(|m| m.len())
        };
        dir_entries.push(DirEntry {
            name: file_name,
            is_dir,
            location: EntryLocation::Fs(entry.path()),
            size,
        });
    }

    dir_entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

    if path.parent().is_some() {
        entries.push(DirEntry {
            name: "..".to_string(),
            is_dir: true,
            location: EntryLocation::Fs(path.parent().unwrap().to_path_buf()),
            size: None,
        });
    }

    entries.extend(dir_entries);

    Ok(entries)
}

fn read_zip_directory(archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::new(file);
    let mut zip = zip::ZipArchive::new(reader)?;
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
            location: EntryLocation::Container {
                kind: ContainerKind::Zip,
                archive_path: archive_path.to_path_buf(),
                inner_path: parent,
            },
            size: None,
        });
    } else {
        let parent = archive_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        entries.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            location: EntryLocation::Fs(parent),
            size: None,
        });
    }

    let mut dir_entries: Vec<DirEntry> = dirs
        .into_iter()
        .map(|d| DirEntry {
            name: d.clone(),
            is_dir: true,
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
        })
        .collect();

    let mut file_entries: Vec<DirEntry> = files
        .into_iter()
        .map(|f| DirEntry {
            name: f.clone(),
            is_dir: false,
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
        })
        .collect();

    dir_entries.sort_by(|a, b| a.name.cmp(&b.name));
    file_entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries.extend(dir_entries);
    entries.extend(file_entries);

    Ok(entries)
}

pub fn format_preview_info(kind: &str, location: &EntryLocation) -> String {
    match location {
        EntryLocation::Fs(path) => format!("{kind}\n{}", path.to_string_lossy()),
        EntryLocation::Container {
            kind: container_kind,
            archive_path,
            inner_path,
        } => {
            let display = container_display_path(*container_kind, archive_path, inner_path);
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

pub fn is_image_name(name: &str) -> bool {
    is_image_path(Path::new(name))
}

pub fn is_audio_path(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase()),
        Some(ext)
            if matches!(
                ext.as_str(),
                "mp3" | "wav" | "flac" | "ogg" | "opus" | "m4a" | "aac" | "alac"
                    | "aiff" | "wma"
            )
    )
}

pub fn is_audio_name(name: &str) -> bool {
    is_audio_path(Path::new(name))
}

pub fn is_video_path(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase()),
        Some(ext)
            if matches!(
                ext.as_str(),
                "mp4"
                    | "m4v"
                    | "mkv"
                    | "avi"
                    | "mov"
                    | "webm"
                    | "mpg"
                    | "mpeg"
                    | "flv"
                    | "wmv"
            )
    )
}

pub fn is_video_name(name: &str) -> bool {
    is_video_path(Path::new(name))
}

pub fn is_media_name(name: &str) -> bool {
    is_image_name(name) || is_audio_name(name) || is_video_name(name)
}

pub fn is_text_path(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase()),
        Some(ext)
            if matches!(
                ext.as_str(),
                "txt"
                    | "md"
                    | "json"
                    | "toml"
                    | "yaml"
                    | "yml"
                    | "rs"
                    | "log"
                    | "ini"
                    | "csv"
                    | "nix"
            )
    )
}

pub fn is_text_name(name: &str) -> bool {
    is_text_path(Path::new(name))
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
        let mode_str = size.as_ref().and_then(|(_, mode)| mode.map(format_mode));
        let size_str = size.as_ref().map(|(size, _)| size.as_str());

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
    match &entry.location {
        EntryLocation::Container { inner_path, .. } => inner_path,
        _ => &entry.name,
    }
}

pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1}G", b / GB)
    } else if b >= MB {
        format!("{:.1}M", b / MB)
    } else if b >= KB {
        format!("{:.1}K", b / KB)
    } else {
        format!("{}B", bytes)
    }
}

fn format_mode(mode: u32) -> String {
    let file_type = if mode & 0o40000 != 0 {
        'd'
    } else if mode & 0o120000 != 0 {
        'l'
    } else {
        '-'
    };
    let mut out = String::with_capacity(10);
    out.push(file_type);
    let perms = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    for (mask, ch) in perms {
        if mode & mask != 0 {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out
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

fn read_zip_bytes_prefix(
    archive_path: &Path,
    inner_path: &str,
    max_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::new(file);
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

fn read_tar_gz_directory(archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::new(file);
    let decoder = flate2::read::GzDecoder::new(reader);
    let mut archive = tar::Archive::new(decoder);
    let mut dirs: HashSet<String> = HashSet::new();
    let mut files: Vec<String> = Vec::new();

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
            location: EntryLocation::Container {
                kind: ContainerKind::TarGz,
                archive_path: archive_path.to_path_buf(),
                inner_path: parent,
            },
            size: None,
        });
    } else {
        let parent = archive_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        entries.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            location: EntryLocation::Fs(parent),
            size: None,
        });
    }

    let mut dir_entries: Vec<DirEntry> = dirs
        .into_iter()
        .map(|d| DirEntry {
            name: d.clone(),
            is_dir: true,
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
        })
        .collect();

    let mut file_entries: Vec<DirEntry> = files
        .into_iter()
        .map(|f| DirEntry {
            name: f.clone(),
            is_dir: false,
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
        })
        .collect();

    dir_entries.sort_by(|a, b| a.name.cmp(&b.name));
    file_entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries.extend(dir_entries);
    entries.extend(file_entries);

    Ok(entries)
}

fn read_tar_gz_bytes_prefix(
    archive_path: &Path,
    inner_path: &str,
    max_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::new(file);
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
    let mut s = path.to_string_lossy().replace('\\', "/");
    while s.starts_with("./") {
        s = s[2..].to_string();
    }
    s.trim_start_matches('/').to_string()
}

fn read_tar_bz2_directory_with_progress(
    archive_path: &Path,
    cwd: &str,
    progress: &mut dyn FnMut(usize),
) -> anyhow::Result<Vec<DirEntry>> {
    let file = fs::File::open(archive_path)?;
    let reader = std::io::BufReader::new(file);
    let decoder = bzip2::read::BzDecoder::new(reader);
    let mut archive = tar::Archive::new(decoder);
    let mut dirs: HashSet<String> = HashSet::new();
    let mut files: Vec<String> = Vec::new();
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
            if seen % PROGRESS_INTERVAL == 0 {
                progress(seen);
            }
            continue;
        }
        let rem = &name[prefix.len()..];
        if rem.is_empty() {
            seen += 1;
            if seen % PROGRESS_INTERVAL == 0 {
                progress(seen);
            }
            continue;
        }
        if let Some(slash) = rem.find('/') {
            let dir = rem[..slash].to_string();
            dirs.insert(dir);
        } else {
            files.push(rem.to_string());
        }
        seen += 1;
        if seen % PROGRESS_INTERVAL == 0 {
            progress(seen);
        }
    }
    progress(seen);

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
            location: EntryLocation::Container {
                kind: ContainerKind::TarBz2,
                archive_path: archive_path.to_path_buf(),
                inner_path: parent,
            },
            size: None,
        });
    } else {
        let parent = archive_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        entries.push(DirEntry {
            name: "..".into(),
            is_dir: true,
            location: EntryLocation::Fs(parent),
            size: None,
        });
    }

    let mut dir_entries: Vec<DirEntry> = dirs
        .into_iter()
        .map(|d| DirEntry {
            name: d.clone(),
            is_dir: true,
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
        })
        .collect();

    let mut file_entries: Vec<DirEntry> = files
        .into_iter()
        .map(|f| DirEntry {
            name: f.clone(),
            is_dir: false,
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
        })
        .collect();

    dir_entries.sort_by(|a, b| a.name.cmp(&b.name));
    file_entries.sort_by(|a, b| a.name.cmp(&b.name));
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
    let reader = std::io::BufReader::new(file);
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
        let reader = std::io::BufReader::new(file);
        let decoder = flate2::read::GzDecoder::new(reader);
        let mut archive = tar::Archive::new(decoder);
        let normalized = inner_path.trim_start_matches('/');
        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?;
            let name = normalize_archive_path(&path);
            if name == normalized {
                let size = entry.size();
                let mode = entry.header().mode().ok().map(|v| v as u32);
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
        let reader = std::io::BufReader::new(file);
        let decoder = bzip2::read::BzDecoder::new(reader);
        let mut archive = tar::Archive::new(decoder);
        let normalized = inner_path.trim_start_matches('/');
        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?;
            let name = normalize_archive_path(&path);
            if name == normalized {
                let size = entry.size();
                let mode = entry.header().mode().ok().map(|v| v as u32);
                return Ok(Some((size, mode)));
            }
        }
        Ok(None)
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
