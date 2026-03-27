use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::TcpStream,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
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
    session.set_timeout(30_000);

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
    })
    .map_err(|(msg, _)| msg)?;
    Ok(all)
}

/// Incrementally list a remote directory, calling `on_batch` for each batch of entries.
/// The first batch always contains the ".." entry (if applicable).
/// Entries within each batch are unsorted; the final sort is the caller's responsibility.
/// Returns `Err((message, is_connection_error))`.
/// `is_connection_error = true` means the SSH session is likely dead (timeout, disconnect).
/// `is_connection_error = false` means an SFTP-level error (permission denied, etc.)
pub fn read_directory_streaming(
    sftp: &Sftp,
    host: &str,
    path: &str,
    mut on_batch: impl FnMut(Vec<DirEntry>),
) -> Result<(), (String, bool)> {
    let remote_path = if path.is_empty() { "/" } else { path };
    let mut handle = sftp.opendir(Path::new(remote_path)).map_err(|e| {
        let is_connection_error = !matches!(e.code(), ssh2::ErrorCode::SFTP(_));
        (format!("opendir {remote_path}: {e}"), is_connection_error)
    })?;

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
/// Reports each deleted item via `progress.add_item()` when provided.
pub fn recursive_delete(
    sftp: &Sftp,
    path: &str,
    is_dir: bool,
    progress: Option<&crate::core::TransferProgress>,
) -> Result<(), String> {
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
            recursive_delete(sftp, &child_str, stat.is_dir(), progress)?;
        }
        sftp.rmdir(Path::new(path))
            .map_err(|e| format!("rmdir {path}: {e}"))?;
    } else {
        sftp.unlink(Path::new(path))
            .map_err(|e| format!("unlink {path}: {e}"))?;
    }
    if let Some(p) = progress {
        p.add_item();
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

/// Copy a file within the same remote host (read then write).
pub fn copy_remote_remote(sftp: &Sftp, src_path: &str, dst_path: &str) -> Result<(), String> {
    let data = read_file_full(sftp, src_path)?;
    write_file(sftp, dst_path, &data)
}

/// Recursively copy a file or directory within the same remote host.
pub fn recursive_copy_remote(
    sftp: &Sftp,
    src_path: &str,
    dst_dir: &str,
    name: &str,
) -> Result<(), String> {
    let dst_path = format!("{}/{}", dst_dir.trim_end_matches('/'), name);
    let stat = sftp
        .stat(Path::new(src_path))
        .map_err(|e| format!("stat {src_path}: {e}"))?;
    if stat.is_dir() {
        sftp.mkdir(Path::new(&dst_path), 0o755)
            .map_err(|e| format!("mkdir {dst_path}: {e}"))?;
        let children = sftp
            .readdir(Path::new(src_path))
            .map_err(|e| format!("readdir {src_path}: {e}"))?;
        for (child_path, _) in children {
            let child_name = child_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if child_name == "." || child_name == ".." {
                continue;
            }
            let child_src = format!("{}/{}", src_path.trim_end_matches('/'), child_name);
            recursive_copy_remote(sftp, &child_src, &dst_path, child_name)?;
        }
    } else {
        copy_remote_remote(sftp, src_path, &dst_path)?;
    }
    Ok(())
}

/// A `Read` wrapper that tracks transferred bytes and checks a cancel flag.
struct TrackedReader<'a, R: Read> {
    inner: R,
    cancel: &'a AtomicBool,
    progress: Option<&'a crate::core::TransferProgress>,
}
impl<R: Read> Read for TrackedReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("Cancelled"));
        }
        let n = self.inner.read(buf)?;
        if let Some(p) = self.progress {
            p.add(n as u64);
        }
        Ok(n)
    }
}

/// A `Write` wrapper that tracks transferred bytes and checks a cancel flag.
struct TrackedWriter<'a, W: Write> {
    inner: W,
    cancel: &'a AtomicBool,
    progress: Option<&'a crate::core::TransferProgress>,
}
impl<W: Write> Write for TrackedWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("Cancelled"));
        }
        let n = self.inner.write(buf)?;
        if let Some(p) = self.progress {
            p.add(n as u64);
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn is_cancel_err(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Other && e.to_string() == "Cancelled"
}

/// Copy a remote directory tree to a local path.
/// Runs `tar cf -` on the remote via SSH exec, extracts locally with the Rust `tar` crate.
pub fn copy_remote_dir_to_local_via_tar(
    src_session: &Session,
    src_path: &str,
    dst_dir: &std::path::Path,
    name: &str,
    cancel: &AtomicBool,
    progress: Option<&crate::core::TransferProgress>,
) -> Result<(), String> {
    let (src_parent, src_name) = match src_path.rfind('/') {
        Some(pos) => (&src_path[..pos], &src_path[pos + 1..]),
        None => (".", src_path),
    };
    let src_parent = if src_parent.is_empty() {
        "/"
    } else {
        src_parent
    };

    let src_cmd = format!(
        "tar cf - -C {} {}",
        sh_quote(src_parent),
        sh_quote(src_name)
    );
    let mut src_ch = src_session
        .channel_session()
        .map_err(|e| format!("src channel_session: {e}"))?;
    src_ch
        .exec(&src_cmd)
        .map_err(|e| format!("src exec: {e}"))?;

    let buf = io::BufReader::with_capacity(1 << 20, &mut src_ch);
    let reader = TrackedReader {
        inner: buf,
        cancel,
        progress,
    };
    let mut archive = tar::Archive::new(reader);
    archive.unpack(dst_dir).map_err(|e| {
        if e.get_ref().is_some_and(|s| {
            is_cancel_err(
                s.downcast_ref::<io::Error>()
                    .unwrap_or(&io::Error::other("")),
            )
        }) {
            "Cancelled".to_string()
        } else {
            format!("tar extract: {e}")
        }
    })?;

    if name != src_name {
        std::fs::rename(dst_dir.join(src_name), dst_dir.join(name))
            .map_err(|e| format!("rename: {e}"))?;
    }
    Ok(())
}

/// Copy a local directory tree to a remote path.
/// Creates the tar archive with the Rust `tar` crate, extracts remotely via SSH exec `tar xf -`.
pub fn copy_local_dir_to_remote_via_tar(
    src_path: &std::path::Path,
    dst_session: &Session,
    dst_dir: &str,
    cancel: &AtomicBool,
    progress: Option<&crate::core::TransferProgress>,
) -> Result<(), String> {
    let src_name = src_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("dir");

    let dst_cmd = format!("tar xf - -C {}", sh_quote(dst_dir));
    let mut dst_ch = dst_session
        .channel_session()
        .map_err(|e| format!("dst channel_session: {e}"))?;
    dst_ch
        .exec(&dst_cmd)
        .map_err(|e| format!("dst exec: {e}"))?;

    let buf = io::BufWriter::with_capacity(1 << 20, &mut dst_ch);
    let mut writer = TrackedWriter {
        inner: buf,
        cancel,
        progress,
    };
    let result = (|| -> io::Result<()> {
        let mut ar = tar::Builder::new(&mut writer);
        ar.append_dir_all(src_name, src_path)?;
        ar.finish()
    })();

    // Drop writer (and its BufWriter) to flush remaining data and release the &mut dst_ch borrow.
    drop(writer);

    match result {
        Ok(()) => {}
        Err(e) if is_cancel_err(&e) => return Err("Cancelled".to_string()),
        Err(e) => return Err(format!("tar create: {e}")),
    }

    dst_ch.send_eof().map_err(|e| format!("send_eof: {e}"))?;
    dst_ch
        .wait_close()
        .map_err(|e| format!("dst wait_close: {e}"))?;
    let exit = dst_ch.exit_status().unwrap_or(-1);
    if exit != 0 {
        return Err(format!("remote tar xf exited with status {exit}"));
    }
    Ok(())
}

/// Shell-quote a string with single quotes, escaping any internal single quotes.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Return the total byte size of a remote path via SSH exec.
/// Far faster than recursive SFTP readdir for large trees (one round-trip vs O(dirs)).
///
/// Strategy (in order):
///   1. `du -sb`  — Linux/GNU coreutils: exact bytes.
///   2. `du -sk`  — macOS/BSD POSIX du: 1 KiB blocks → multiply by 1024.
///   3. Return 0  — Windows SSH or other exotic remote; progress bar shows animated form.
pub fn count_bytes_via_exec(session: &Session, path: &str) -> u64 {
    let quoted = sh_quote(path);
    for (cmd, scale) in [
        (format!("du -sb {quoted} 2>/dev/null"), 1u64),
        (format!("du -sk {quoted} 2>/dev/null"), 1024u64),
    ] {
        let mut ch = match session.channel_session() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if ch.exec(&cmd).is_err() {
            let _ = ch.wait_close();
            continue;
        }
        let mut out = String::new();
        let _ = ch.read_to_string(&mut out);
        let _ = ch.wait_close();
        let ok = ch.exit_status().unwrap_or(1) == 0;
        if ok {
            // du output: "12345\t/path/name\n" — first token is the numeric value
            if let Some(n) = out
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u64>().ok())
            {
                return n * scale;
            }
        }
    }
    0
}

/// Return the total byte size of all regular files under `path` on the local filesystem.
pub fn count_bytes_local(path: &std::path::Path) -> u64 {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| count_bytes_local(&e.path()))
                    .sum()
            })
            .unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// Copy a file or directory tree between two different remote hosts using a single
/// `tar cf -` → relay → `tar xf -` stream.  This avoids per-file SFTP round-trips.
pub fn copy_cross_host_via_tar(
    src_session: &Session,
    src_path: &str,
    dst_session: &Session,
    dst_dir: &str,
    name: &str,
    cancel: &AtomicBool,
    progress: Option<&crate::core::TransferProgress>,
) -> Result<(), String> {
    let (src_parent, src_name) = match src_path.rfind('/') {
        Some(pos) => (&src_path[..pos], &src_path[pos + 1..]),
        None => (".", src_path),
    };
    let src_parent = if src_parent.is_empty() {
        "/"
    } else {
        src_parent
    };

    let src_cmd = format!(
        "tar cf - -C {} {}",
        sh_quote(src_parent),
        sh_quote(src_name)
    );
    let dst_cmd = format!("tar xf - -C {}", sh_quote(dst_dir));

    let mut src_ch = src_session
        .channel_session()
        .map_err(|e| format!("src channel_session: {e}"))?;
    src_ch
        .exec(&src_cmd)
        .map_err(|e| format!("src exec '{src_cmd}': {e}"))?;

    let mut dst_ch = dst_session
        .channel_session()
        .map_err(|e| format!("dst channel_session: {e}"))?;
    dst_ch
        .exec(&dst_cmd)
        .map_err(|e| format!("dst exec '{dst_cmd}': {e}"))?;

    // Relay compressed tar stream from source to destination.
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".to_string());
        }
        match src_ch.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                dst_ch
                    .write_all(&buf[..n])
                    .map_err(|e| format!("relay write: {e}"))?;
                if let Some(p) = progress {
                    p.add(n as u64);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("relay read: {e}")),
        }
    }

    dst_ch.send_eof().map_err(|e| format!("send_eof: {e}"))?;
    dst_ch
        .wait_close()
        .map_err(|e| format!("dst wait_close: {e}"))?;
    let exit = dst_ch.exit_status().unwrap_or(-1);
    if exit != 0 {
        return Err(format!("tar xzf exited with status {exit}"));
    }

    // Rename on destination if the target name differs from the source name.
    if name != src_name {
        let mv_cmd = format!(
            "mv {} {}",
            sh_quote(&format!("{}/{}", dst_dir.trim_end_matches('/'), src_name)),
            sh_quote(&format!("{}/{}", dst_dir.trim_end_matches('/'), name)),
        );
        let mut mv_ch = dst_session
            .channel_session()
            .map_err(|e| format!("mv channel: {e}"))?;
        mv_ch.exec(&mv_cmd).map_err(|e| format!("mv exec: {e}"))?;
        mv_ch
            .wait_close()
            .map_err(|e| format!("mv wait_close: {e}"))?;
        let mv_exit = mv_ch.exit_status().unwrap_or(-1);
        if mv_exit != 0 {
            return Err(format!("mv exited with status {mv_exit}"));
        }
    }

    Ok(())
}

/// Count the total byte size of a remote path (file or directory tree).
pub fn count_bytes_remote(sftp: &Sftp, path: &str) -> u64 {
    match sftp.stat(Path::new(path)) {
        Ok(stat) if stat.is_dir() => {
            let children = sftp.readdir(Path::new(path)).unwrap_or_default();
            children
                .into_iter()
                .filter_map(|(child_path, _)| {
                    let name = child_path.file_name().and_then(|s| s.to_str())?;
                    if name == "." || name == ".." {
                        return None;
                    }
                    Some(count_bytes_remote(sftp, &child_path.to_string_lossy()))
                })
                .sum()
        }
        Ok(stat) => stat.size.unwrap_or(0),
        Err(_) => 0,
    }
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
