use std::{
    fs,
    io::{self, Read},
    path::{self, Path},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::UNIX_EPOCH,
};

/// Shared transfer progress, updated atomically by worker threads and read by
/// the UI. One instance lives in AppState behind an Arc.
pub struct TransferProgress {
    /// Bytes transferred so far.
    pub bytes_done: AtomicU64,
    /// Total bytes expected (0 = unknown).
    pub bytes_total: AtomicU64,
}

impl TransferProgress {
    pub fn new() -> Self {
        Self {
            bytes_done: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
        }
    }

    pub fn reset(&self, total: u64) {
        self.bytes_done.store(0, Ordering::Relaxed);
        self.bytes_total.store(total, Ordering::Relaxed);
    }

    pub fn add(&self, n: u64) {
        self.bytes_done.fetch_add(n, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.bytes_done.load(Ordering::Relaxed),
            self.bytes_total.load(Ordering::Relaxed),
        )
    }
}

pub use crate::archive::{
    ContainerKind, container_display_path, container_kind_from_path, copy_container_dir,
    copy_container_entry, create_archive, format_container_listing, is_container_path,
    normalize_archive_path, read_container_bytes_prefix, read_container_directory,
    read_container_directory_with_progress, read_container_metadata,
};

#[derive(Clone)]
pub enum EntryLocation {
    Fs(path::PathBuf),
    Container {
        kind: ContainerKind,
        archive_path: path::PathBuf,
        inner_path: String, // no leading slash, '' means root
    },
    Remote {
        host: String,
        path: String, // absolute path on remote, e.g. "/home/user"
    },
}

impl EntryLocation {
    pub fn display_name(&self) -> String {
        match *self {
            EntryLocation::Fs(ref path) => path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("<unknown>")
                .to_string(),
            EntryLocation::Container { ref inner_path, .. } => inner_path
                .rsplit('/')
                .next()
                .unwrap_or("<unknown>")
                .to_string(),
            EntryLocation::Remote { ref path, .. } => {
                path.rsplit('/').next().unwrap_or("<unknown>").to_string()
            }
        }
    }
}

#[derive(Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub link_target: Option<String>,
    pub location: EntryLocation,
    pub size: Option<u64>,
    pub modified: Option<u64>,
}

pub enum DirBatch {
    Append(Vec<DirEntry>),
    Replace(Vec<DirEntry>),
    ContainerRoot(Option<String>),
    Loading,
    Progress { loaded: usize, total: Option<usize> },
    Error(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ActivePanel {
    Left,
    Right,
}

#[derive(Clone)]
pub enum BrowserMode {
    Fs,
    Container {
        kind: ContainerKind,
        archive_path: path::PathBuf,
        cwd: String,
        root: Option<String>,
    },
    Search {
        root: path::PathBuf,
        query: String,
        mode: SearchMode,
        case: SearchCase,
    },
    Remote {
        host: String,
        path: String,
    },
}

pub enum PreviewContent {
    Text(String),
    Binary(Vec<u8>),
    TextChunk { text: String, done: bool },
    BinaryChunk { data: Vec<u8>, done: bool },
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
    Remote {
        host: String,
        path: String,
    },
}

pub enum PreviewRequest {
    Read {
        id: u64,
        location: EntryLocation,
        max_bytes: Option<usize>,
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
    WriteFile {
        path: path::PathBuf,
        contents: Vec<u8>,
    },
    Mkdir {
        path: path::PathBuf,
    },
    SetProps {
        path: path::PathBuf,
        mode: u32,
        uid: u32,
        gid: u32,
        recursive: bool,
    },
    Pack {
        sources: Vec<path::PathBuf>,
        archive_path: path::PathBuf,
        kind: crate::archive::ContainerKind,
    },
    WriteRemoteFile {
        host: String,
        path: String,
        contents: Vec<u8>,
    },
    CopyRemoteToLocal {
        host: String,
        remote_path: String,
        dst_dir: path::PathBuf,
        name: String,
        is_dir: bool,
    },
    CopyLocalToRemote {
        src: path::PathBuf,
        host: String,
        remote_dir: String,
        is_dir: bool,
    },
    DeleteRemote {
        host: String,
        path: String,
        is_dir: bool,
    },
    RenameRemote {
        host: String,
        src: String,
        new_name: String,
    },
    MkdirRemote {
        host: String,
        path: String,
    },
    CopyRemoteToLocalAndOpen {
        host: String,
        remote_path: String,
        local_path: path::PathBuf,
    },
    CopyRemoteSameHost {
        host: String,
        src_path: String,
        dst_dir: String,
        name: String,
    },
    MoveRemoteSameHost {
        host: String,
        src_path: String,
        dst_dir: String,
        name: String,
    },
    CopyContainerAndOpen {
        kind: crate::archive::ContainerKind,
        archive_path: path::PathBuf,
        inner_path: String,
        dst_dir: path::PathBuf,
        display_name: String,
    },
    CopyRemoteCrossHost {
        src_host: String,
        src_path: String,
        dst_host: String,
        dst_dir: String,
        name: String,
        is_dir: bool,
    },
}

pub enum IOResult {
    /// Refresh all local (Fs) panels — default for local ops.
    Completed,
    /// Refresh only remote panels browsing this host.
    CompletedRemote(String),
    /// No panel refresh needed (open-only / read-only ops).
    CompletedSilent,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    Name,
    Date,
    Size,
    Raw,
}

pub struct EditLoadRequest {
    pub id: u64,
    pub path: path::PathBuf,
    pub remote: Option<(String, String)>, // (host, remote_path)
}

pub struct EditLoadResult {
    pub id: u64,
    pub path: path::PathBuf,
    pub text: String,
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
    pub modified: Option<u64>,
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

pub fn read_fs_directory(path: &path::Path) -> anyhow::Result<Vec<DirEntry>> {
    let mut entries = Vec::new();

    let read_dir = fs::read_dir(path)?;
    let mut dir_entries = Vec::new();

    for entry in read_dir {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy().to_string();

        let file_type = entry.file_type()?;
        let is_symlink = file_type.is_symlink();
        // DirEntry::metadata() uses lstat (no follow); fs::metadata follows symlinks
        let metadata = if is_symlink {
            fs::metadata(entry.path()).ok()
        } else {
            entry.metadata().ok()
        };
        let is_dir = if is_symlink {
            metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false)
        } else {
            file_type.is_dir()
        };
        let size = if is_dir {
            None
        } else {
            metadata.as_ref().map(|m| m.len())
        };
        let modified = metadata
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        dir_entries.push(DirEntry {
            name: file_name,
            is_dir,
            is_symlink,
            link_target: None,
            location: EntryLocation::Fs(entry.path()),
            size,
            modified,
        });
    }

    dir_entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

    if path.parent().is_some() {
        entries.push(DirEntry {
            name: "..".to_string(),
            is_dir: true,
            is_symlink: false,
            link_target: None,
            location: EntryLocation::Fs(path.parent().unwrap().to_path_buf()),
            size: None,
            modified: None,
        });
    }

    entries.extend(dir_entries);

    Ok(entries)
}

pub fn format_preview_info(kind: &str, location: &EntryLocation) -> String {
    match *location {
        EntryLocation::Fs(ref path) => format!("{kind}\n{}", path.to_string_lossy()),
        EntryLocation::Container {
            kind: container_kind,
            ref archive_path,
            ref inner_path,
        } => {
            let display = container_display_path(container_kind, archive_path, inner_path);
            format!("{kind}\n{display}")
        }
        EntryLocation::Remote { ref host, ref path } => format!("{kind}\n{host}:{path}"),
    }
}

pub fn is_image_path(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase()),
        Some(ext) if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tga" | "hdr" | "dds")
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

pub fn format_mode(mode: u32) -> String {
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

pub fn hexdump(bytes: &[u8]) -> String {
    hexdump_with_width(bytes, 16)
}

pub fn hexdump_with_width(bytes: &[u8], width: usize) -> String {
    let width = width.clamp(4, 32);
    let mut out = String::new();
    let mut offset = 0usize;
    for chunk in bytes.chunks(width) {
        out.push_str(&format!("{:08x}: ", offset));
        for i in 0..width {
            if i < chunk.len() {
                out.push_str(&format!("{:02x} ", chunk[i]));
            } else {
                out.push_str("   ");
            }
            if i == (width / 2).saturating_sub(1) {
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
        offset += width;
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
    // Strip UTF-8 BOM if present
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    // Valid UTF-8 is almost certainly text
    if std::str::from_utf8(bytes).is_ok() {
        return true;
    }
    // Fall back to printable ASCII ratio for non-UTF-8 encodings
    let mut printable = 0usize;
    for &b in bytes {
        match b {
            0x09 | 0x0A | 0x0D => printable += 1,
            0x20..=0x7E => printable += 1,
            0x80..=0xFF => printable += 1, // high bytes (Latin-1, etc.)
            _ => {}
        }
    }
    let ratio = printable as f32 / bytes.len().max(1) as f32;
    ratio > 0.85
}
