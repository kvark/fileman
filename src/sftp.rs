use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::TcpStream,
    path::Path,
};

use ssh2::{self, Session, Sftp};

use crate::core::{DirEntry, EntryLocation};

pub struct SftpSession {
    pub session: Session,
    pub sftp: Sftp,
    pub host: String,
}

pub struct SshHostConfig {
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Vec<String>,
}

/// Parse `~/.ssh/config` for Host/Hostname/User/Port/IdentityFile.
pub fn parse_ssh_config(content: &str) -> HashMap<String, SshHostConfig> {
    let mut hosts: HashMap<String, SshHostConfig> = HashMap::new();
    let mut current_hosts: Vec<String> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Split on first whitespace or '='
        let (key, value) = if let Some(eq) = trimmed.find('=') {
            let (k, v) = trimmed.split_at(eq);
            (k.trim(), v[1..].trim())
        } else if let Some(sp) = trimmed.find(char::is_whitespace) {
            let (k, v) = trimmed.split_at(sp);
            (k.trim(), v.trim())
        } else {
            continue;
        };

        match key.to_ascii_lowercase().as_str() {
            "host" => {
                current_hosts.clear();
                for h in value.split_whitespace() {
                    if h.contains('*') || h.contains('?') {
                        continue;
                    }
                    current_hosts.push(h.to_string());
                    hosts.entry(h.to_string()).or_insert_with(|| SshHostConfig {
                        hostname: None,
                        user: None,
                        port: None,
                        identity_files: Vec::new(),
                    });
                }
            }
            "hostname" => {
                for h in &current_hosts {
                    if let Some(cfg) = hosts.get_mut(h) {
                        cfg.hostname = Some(value.to_string());
                    }
                }
            }
            "user" => {
                for h in &current_hosts {
                    if let Some(cfg) = hosts.get_mut(h) {
                        cfg.user = Some(value.to_string());
                    }
                }
            }
            "port" => {
                if let Ok(port) = value.parse::<u16>() {
                    for h in &current_hosts {
                        if let Some(cfg) = hosts.get_mut(h) {
                            cfg.port = Some(port);
                        }
                    }
                }
            }
            "identityfile" => {
                let expanded = expand_tilde(value);
                for h in &current_hosts {
                    if let Some(cfg) = hosts.get_mut(h) {
                        cfg.identity_files.push(expanded.clone());
                    }
                }
            }
            _ => {}
        }
    }
    hosts
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    path.to_string()
}

/// Connect to an SSH host using config resolution. Tries ssh-agent, then key files.
pub fn connect(
    host: &str,
    ssh_config: &HashMap<String, SshHostConfig>,
) -> Result<SftpSession, String> {
    let config = ssh_config.get(host);
    let actual_host = config.and_then(|c| c.hostname.as_deref()).unwrap_or(host);
    let user = config
        .and_then(|c| c.user.as_deref())
        .map(|s| s.to_string())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "root".to_string());
    let port = config.and_then(|c| c.port).unwrap_or(22);

    let addr = format!("{actual_host}:{port}");
    let tcp = TcpStream::connect(&addr).map_err(|e| format!("TCP connect to {addr}: {e}"))?;
    tcp.set_nodelay(true).ok();

    let mut session = Session::new().map_err(|e| format!("SSH session init: {e}"))?;
    session.set_tcp_stream(tcp);
    session
        .handshake()
        .map_err(|e| format!("SSH handshake with {actual_host}: {e}"))?;

    // Try ssh-agent first
    if session.userauth_agent(&user).is_ok() && session.authenticated() {
        let sftp = session.sftp().map_err(|e| format!("SFTP subsystem: {e}"))?;
        return Ok(SftpSession {
            session,
            sftp,
            host: host.to_string(),
        });
    }

    // Try key files from config, then default paths
    let mut key_paths: Vec<String> = config.map(|c| c.identity_files.clone()).unwrap_or_default();
    if let Ok(home) = std::env::var("HOME") {
        for default in &["id_ed25519", "id_rsa", "id_ecdsa"] {
            let path = format!("{home}/.ssh/{default}");
            if !key_paths.contains(&path) {
                key_paths.push(path);
            }
        }
    }

    for key_path in &key_paths {
        let key = Path::new(key_path);
        if !key.exists() {
            continue;
        }
        if session.userauth_pubkey_file(&user, None, key, None).is_ok() && session.authenticated() {
            let sftp = session.sftp().map_err(|e| format!("SFTP subsystem: {e}"))?;
            return Ok(SftpSession {
                session,
                sftp,
                host: host.to_string(),
            });
        }
    }

    Err(format!(
        "Authentication failed for {user}@{actual_host}:{port}. \
         Ensure ssh-agent is running or key files are available."
    ))
}

/// List a remote directory, producing DirEntry items with EntryLocation::Remote.
/// Does not include ".." when path is "/".
pub fn read_directory(sftp: &Sftp, host: &str, path: &str) -> Result<Vec<DirEntry>, String> {
    let mut all = Vec::new();
    read_directory_streaming(sftp, host, path, |entries| {
        all.extend(entries);
    })?;
    Ok(all)
}

/// Incrementally list a remote directory, calling `on_batch` for each batch of entries.
/// The first batch always contains the ".." entry (if applicable).
/// Entries within each batch are unsorted; the final sort is the caller's responsibility.
pub fn read_directory_streaming(
    sftp: &Sftp,
    host: &str,
    path: &str,
    mut on_batch: impl FnMut(Vec<DirEntry>),
) -> Result<(), String> {
    let remote_path = if path.is_empty() { "/" } else { path };
    let mut handle = sftp
        .opendir(Path::new(remote_path))
        .map_err(|e| format!("opendir {remote_path}: {e}"))?;

    // First batch: ".." entry if not at root
    if remote_path != "/" {
        let parent = parent_remote_path(remote_path);
        on_batch(vec![DirEntry {
            name: "..".to_string(),
            is_dir: true,
            is_symlink: false,
            link_target: None,
            location: EntryLocation::Remote {
                host: host.to_string(),
                path: parent,
            },
            size: None,
            modified: None,
        }]);
    }

    const BATCH_SIZE: usize = 64;
    let mut batch = Vec::with_capacity(BATCH_SIZE);

    while let Ok((pathbuf, stat)) = handle.readdir() {
        let name = pathbuf
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        let is_dir = stat.is_dir();
        let is_symlink = stat.file_type() == ssh2::FileType::Symlink;
        let size = if is_dir { None } else { stat.size };
        let modified = stat.mtime;
        let inner_path = if remote_path == "/" {
            format!("/{name}")
        } else {
            format!("{remote_path}/{name}")
        };
        let link_target = if is_symlink {
            sftp.readlink(Path::new(&inner_path))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            None
        };
        batch.push(DirEntry {
            name,
            is_dir,
            is_symlink,
            link_target,
            location: EntryLocation::Remote {
                host: host.to_string(),
                path: inner_path,
            },
            size,
            modified,
        });
        if batch.len() >= BATCH_SIZE {
            on_batch(std::mem::replace(
                &mut batch,
                Vec::with_capacity(BATCH_SIZE),
            ));
        }
    }

    if !batch.is_empty() {
        on_batch(batch);
    }

    Ok(())
}

/// Read an entire remote file into memory, optionally reporting progress.
pub fn read_file_full(sftp: &Sftp, path: &str) -> Result<Vec<u8>, String> {
    read_file_full_progress(sftp, path, None)
}

/// Read an entire remote file into memory with progress reporting.
pub fn read_file_full_progress(
    sftp: &Sftp,
    path: &str,
    progress: Option<&crate::core::TransferProgress>,
) -> Result<Vec<u8>, String> {
    let stat = sftp.stat(Path::new(path)).ok();
    if let Some(p) = progress {
        p.reset(stat.and_then(|s| s.size).unwrap_or(0));
    }
    let mut file = sftp
        .open(Path::new(path))
        .map_err(|e| format!("open {path}: {e}"))?;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(p) = progress {
                    p.add(n as u64);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("read {path}: {e}")),
        }
    }
    Ok(buf)
}

/// Read a prefix of a remote file for preview purposes.
pub fn read_bytes_prefix(sftp: &Sftp, path: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut file = sftp
        .open(Path::new(path))
        .map_err(|e| format!("open {path}: {e}"))?;
    let mut buf = vec![0u8; max_bytes];
    let mut total = 0;
    while total < max_bytes {
        match file.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("read {path}: {e}")),
        }
    }
    buf.truncate(total);
    Ok(buf)
}

/// Open a remote file as a reader (for streaming preview).
pub fn open_remote_reader(sftp: &Sftp, path: &str) -> Result<ssh2::File, String> {
    sftp.open(Path::new(path))
        .map_err(|e| format!("open {path}: {e}"))
}

/// Recursively delete a remote path (file or directory).
pub fn recursive_delete(sftp: &Sftp, path: &str, is_dir: bool) -> Result<(), String> {
    if is_dir {
        let children = sftp
            .readdir(Path::new(path))
            .map_err(|e| format!("readdir {path}: {e}"))?;
        for (child_path, stat) in children {
            let name = child_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if name == "." || name == ".." {
                continue;
            }
            let child_str = child_path.to_string_lossy().to_string();
            recursive_delete(sftp, &child_str, stat.is_dir())?;
        }
        sftp.rmdir(Path::new(path))
            .map_err(|e| format!("rmdir {path}: {e}"))?;
    } else {
        sftp.unlink(Path::new(path))
            .map_err(|e| format!("unlink {path}: {e}"))?;
    }
    Ok(())
}

/// Write bytes to a remote file (create or overwrite).
pub fn write_file(sftp: &Sftp, path: &str, contents: &[u8]) -> Result<(), String> {
    let mut file = sftp
        .create(Path::new(path))
        .map_err(|e| format!("create {path}: {e}"))?;
    file.write_all(contents)
        .map_err(|e| format!("write {path}: {e}"))?;
    Ok(())
}

/// Create a remote directory.
pub fn mkdir(sftp: &Sftp, path: &str) -> Result<(), String> {
    sftp.mkdir(Path::new(path), 0o755)
        .map_err(|e| format!("mkdir {path}: {e}"))
}

/// Rename a remote file or directory.
pub fn rename(sftp: &Sftp, src: &str, dst: &str) -> Result<(), String> {
    sftp.rename(Path::new(src), Path::new(dst), None)
        .map_err(|e| format!("rename {src} -> {dst}: {e}"))
}

/// Copy a remote file to a local path.
pub fn copy_remote_to_local(
    sftp: &Sftp,
    remote_path: &str,
    local_dst: &Path,
) -> Result<(), String> {
    copy_remote_to_local_progress(sftp, remote_path, local_dst, None)
}

pub fn copy_remote_to_local_progress(
    sftp: &Sftp,
    remote_path: &str,
    local_dst: &Path,
    progress: Option<&crate::core::TransferProgress>,
) -> Result<(), String> {
    let stat = sftp.stat(Path::new(remote_path)).ok();
    if let Some(p) = progress {
        p.reset(stat.and_then(|s| s.size).unwrap_or(0));
    }
    let mut remote_file = sftp
        .open(Path::new(remote_path))
        .map_err(|e| format!("open remote {remote_path}: {e}"))?;
    let mut local_file = std::fs::File::create(local_dst)
        .map_err(|e| format!("create local {}: {e}", local_dst.display()))?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match remote_file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                local_file
                    .write_all(&buf[..n])
                    .map_err(|e| format!("write local: {e}"))?;
                if let Some(p) = progress {
                    p.add(n as u64);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("read remote: {e}")),
        }
    }
    Ok(())
}

/// Copy a local file to a remote path.
pub fn copy_local_to_remote(
    sftp: &Sftp,
    local_src: &Path,
    remote_path: &str,
) -> Result<(), String> {
    copy_local_to_remote_progress(sftp, local_src, remote_path, None)
}

pub fn copy_local_to_remote_progress(
    sftp: &Sftp,
    local_src: &Path,
    remote_path: &str,
    progress: Option<&crate::core::TransferProgress>,
) -> Result<(), String> {
    if let Some(p) = progress {
        let size = std::fs::metadata(local_src).map(|m| m.len()).unwrap_or(0);
        p.reset(size);
    }
    let mut local_file = std::fs::File::open(local_src)
        .map_err(|e| format!("open local {}: {e}", local_src.display()))?;
    let mut remote_file = sftp
        .create(Path::new(remote_path))
        .map_err(|e| format!("create remote {remote_path}: {e}"))?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match local_file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                remote_file
                    .write_all(&buf[..n])
                    .map_err(|e| format!("write remote: {e}"))?;
                if let Some(p) = progress {
                    p.add(n as u64);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("read local: {e}")),
        }
    }
    Ok(())
}

/// Copy a remote file to another path on the same host.
pub fn copy_remote(sftp: &Sftp, src: &str, dst: &str) -> Result<(), String> {
    let mut src_file = sftp
        .open(Path::new(src))
        .map_err(|e| format!("open {src}: {e}"))?;
    let mut dst_file = sftp
        .create(Path::new(dst))
        .map_err(|e| format!("create {dst}: {e}"))?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match src_file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                dst_file
                    .write_all(&buf[..n])
                    .map_err(|e| format!("write {dst}: {e}"))?;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("read {src}: {e}")),
        }
    }
    Ok(())
}

fn parent_remote_path(path: &str) -> String {
    if path == "/" || path.is_empty() {
        return "/".to_string();
    }
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(pos) => trimmed[..pos].to_string(),
        None => "/".to_string(),
    }
}

/// Parse SSH hosts from ~/.ssh/config (cross-platform).
pub fn discover_ssh_hosts() -> Vec<String> {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };
    let config_path = std::path::Path::new(&home).join(".ssh/config");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut hosts = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Host ") {
            for host in rest.split_whitespace() {
                if !host.contains('*') && !host.contains('?') {
                    hosts.push(host.to_string());
                }
            }
        }
    }
    hosts
}

/// Load and parse the SSH config file once.
pub fn load_ssh_config() -> HashMap<String, SshHostConfig> {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return HashMap::new(),
    };
    let config_path = std::path::Path::new(&home).join(".ssh/config");
    match std::fs::read_to_string(&config_path) {
        Ok(content) => parse_ssh_config(&content),
        Err(_) => HashMap::new(),
    }
}
