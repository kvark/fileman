#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::UNIX_EPOCH,
};

use std::sync::{Arc, Mutex};

use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::{
    EntryLocation, IOResult, IOTask, PreviewContent, PreviewRequest, SearchCase, SearchEvent,
    SearchMode, SearchProgress, SearchRequest, SearchResult, copy_container_dir,
    copy_container_entry, copy_recursively, create_archive, format_container_listing,
    is_probably_text, is_text_name, is_text_path, read_container_directory,
};
use crate::sftp::SftpSession;

const PREVIEW_CHUNK_BYTES: usize = 16 * 1024;

pub fn start_io_worker(
    sftp_sessions: Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<SftpSession>>>>>,
    transfer_progress: Arc<crate::core::TransferProgress>,
    wake: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    cancel_flag: Arc<AtomicBool>,
) -> (
    mpsc::Sender<IOTask>,
    mpsc::Receiver<IOResult>,
    mpsc::Sender<()>,
) {
    let (tx, rx) = mpsc::channel::<IOTask>();
    let (result_tx, result_rx) = mpsc::channel::<IOResult>();
    let (cancel_tx, cancel_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut cancel_requested = false;
        while let Ok(task) = rx.recv() {
            while cancel_rx.try_recv().is_ok() {
                cancel_requested = true;
                cancel_flag.store(true, Ordering::Relaxed);
            }
            if cancel_requested {
                let _ = result_tx.send(IOResult::Completed);
                while let Ok(_dropped) = rx.try_recv() {
                    let _ = result_tx.send(IOResult::Completed);
                }
                cancel_requested = false;
                cancel_flag.store(false, Ordering::Relaxed);
                continue;
            }
            cancel_flag.store(false, Ordering::Relaxed);
            transfer_progress.reset(0);
            // Default: refresh local Fs panels. Remote/silent ops override below.
            let mut io_result = IOResult::Completed;
            match task {
                IOTask::Copy { src, dst_dir } => {
                    if let Err(e) = copy_recursively(&src, &dst_dir) {
                        eprintln!("Copy error: {e}");
                    }
                }
                IOTask::CopyContainer {
                    kind,
                    archive_path,
                    inner_path,
                    dst_dir,
                    display_name,
                } => {
                    if let Err(e) = copy_container_entry(
                        kind,
                        &archive_path,
                        &inner_path,
                        &dst_dir,
                        &display_name,
                    ) {
                        eprintln!("Copy container error: {e}");
                    }
                }
                IOTask::CopyContainerDir {
                    kind,
                    archive_path,
                    inner_path,
                    dst_dir,
                    display_name,
                } => {
                    if let Err(e) = copy_container_dir(
                        kind,
                        &archive_path,
                        &inner_path,
                        &dst_dir,
                        &display_name,
                    ) {
                        eprintln!("Copy container dir error: {e}");
                    }
                }
                IOTask::Move { src, dst_dir } => {
                    let target = dst_dir.join(
                        src.file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "moved".to_string()),
                    );
                    if let Err(e) = std::fs::rename(&src, &target) {
                        if let Err(copy_err) = copy_recursively(&src, &dst_dir) {
                            eprintln!("Move error (copy fallback): {copy_err}");
                        } else if let Err(remove_err) = if src.is_dir() {
                            std::fs::remove_dir_all(&src)
                        } else {
                            std::fs::remove_file(&src)
                        } {
                            eprintln!("Move cleanup error: {remove_err}");
                        }
                        eprintln!("Move error: {e}");
                    }
                }
                IOTask::Delete { target } => {
                    let res = if target.is_dir() {
                        std::fs::remove_dir_all(&target)
                    } else {
                        std::fs::remove_file(&target)
                    };
                    if let Err(e) = res {
                        eprintln!("Delete error: {e}");
                    }
                }
                IOTask::Rename { src, new_name } => {
                    let target = src.with_file_name(new_name);
                    if let Err(e) = std::fs::rename(&src, &target) {
                        eprintln!("Rename error: {e}");
                    }
                }
                IOTask::WriteFile { path, contents } => {
                    if let Err(e) = std::fs::write(&path, contents) {
                        eprintln!("Write error: {e}");
                    }
                }
                IOTask::Mkdir { path } => {
                    if let Err(e) = std::fs::create_dir(&path) {
                        eprintln!("Mkdir error: {e}");
                    }
                }
                IOTask::Pack {
                    sources,
                    archive_path,
                    kind,
                } => {
                    if let Err(e) = create_archive(&sources, &archive_path, kind) {
                        eprintln!("Pack error: {e}");
                    }
                }
                #[cfg(unix)]
                IOTask::SetProps {
                    path,
                    mode,
                    uid,
                    gid,
                    recursive,
                } => {
                    let res = if recursive {
                        apply_props_recursive(&path, mode, uid, gid)
                    } else {
                        apply_props(&path, mode, uid, gid)
                    };
                    if let Err(e) = res {
                        eprintln!("Props error: {e}");
                    }
                }
                #[cfg(not(unix))]
                IOTask::SetProps { .. } => {
                    eprintln!("SetProps is not supported on this platform");
                }
                IOTask::WriteRemoteFile {
                    host,
                    path,
                    contents,
                } => {
                    if let Some(session) = sftp_sessions.lock().unwrap().get(&host).cloned() {
                        let locked = session.lock().unwrap();
                        if let Err(e) = crate::sftp::write_file(&locked.sftp, &path, &contents) {
                            eprintln!("Remote write error: {e}");
                        }
                    } else {
                        eprintln!("No SFTP session for host: {host}");
                    }
                    io_result = IOResult::CompletedRemote(host);
                }
                IOTask::CopyRemoteToLocal {
                    host,
                    remote_path,
                    dst_dir,
                    name,
                    is_dir,
                } => {
                    if let Some(session) = sftp_sessions.lock().unwrap().get(&host).cloned() {
                        let locked = session.lock().unwrap();
                        let result = if is_dir {
                            let total =
                                crate::sftp::count_bytes_via_exec(&locked.session, &remote_path);
                            transfer_progress.reset(total);
                            crate::sftp::copy_remote_dir_to_local_via_tar(
                                &locked.session,
                                &remote_path,
                                &dst_dir,
                                &name,
                                &cancel_flag,
                                Some(&transfer_progress),
                            )
                        } else {
                            let local_path = dst_dir.join(&name);
                            crate::sftp::copy_remote_to_local_progress(
                                &locked.sftp,
                                &remote_path,
                                &local_path,
                                Some(&transfer_progress),
                            )
                        };
                        if let Err(e) = result
                            && e != "Cancelled"
                        {
                            eprintln!("Remote-to-local copy error: {e}");
                        }
                    } else {
                        eprintln!("No SFTP session for host: {host}");
                    }
                    // io_result stays Completed: content was added to the local dst
                }
                IOTask::CopyLocalToRemote {
                    src,
                    host,
                    remote_dir,
                    is_dir,
                } => {
                    if let Some(session) = sftp_sessions.lock().unwrap().get(&host).cloned() {
                        let locked = session.lock().unwrap();
                        let result = if is_dir {
                            let total = crate::sftp::count_bytes_local(&src);
                            transfer_progress.reset(total);
                            crate::sftp::copy_local_dir_to_remote_via_tar(
                                &src,
                                &locked.session,
                                &remote_dir,
                                &cancel_flag,
                                Some(&transfer_progress),
                            )
                        } else {
                            let name = src
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_else(|| "file".to_string());
                            let remote_path = format!("{remote_dir}/{name}");
                            crate::sftp::copy_local_to_remote_progress(
                                &locked.sftp,
                                &src,
                                &remote_path,
                                Some(&transfer_progress),
                            )
                        };
                        if let Err(e) = result
                            && e != "Cancelled"
                        {
                            eprintln!("Local-to-remote copy error: {e}");
                        }
                    } else {
                        eprintln!("No SFTP session for host: {host}");
                    }
                    io_result = IOResult::CompletedRemote(host);
                }
                IOTask::DeleteRemote { host, path, is_dir } => {
                    if let Some(session) = sftp_sessions.lock().unwrap().get(&host).cloned() {
                        let locked = session.lock().unwrap();
                        if let Err(e) = crate::sftp::recursive_delete(&locked.sftp, &path, is_dir) {
                            eprintln!("Remote delete error: {e}");
                        }
                    } else {
                        eprintln!("No SFTP session for host: {host}");
                    }
                    io_result = IOResult::CompletedRemote(host);
                }
                IOTask::RenameRemote {
                    host,
                    src,
                    new_name,
                } => {
                    if let Some(session) = sftp_sessions.lock().unwrap().get(&host).cloned() {
                        let locked = session.lock().unwrap();
                        let parent = if let Some(pos) = src.rfind('/') {
                            &src[..pos]
                        } else {
                            ""
                        };
                        let dst = if parent.is_empty() {
                            format!("/{new_name}")
                        } else {
                            format!("{parent}/{new_name}")
                        };
                        if let Err(e) = crate::sftp::rename(&locked.sftp, &src, &dst) {
                            eprintln!("Remote rename error: {e}");
                        }
                    } else {
                        eprintln!("No SFTP session for host: {host}");
                    }
                    io_result = IOResult::CompletedRemote(host);
                }
                IOTask::MkdirRemote { host, path } => {
                    if let Some(session) = sftp_sessions.lock().unwrap().get(&host).cloned() {
                        let locked = session.lock().unwrap();
                        if let Err(e) = crate::sftp::mkdir(&locked.sftp, &path) {
                            eprintln!("Remote mkdir error: {e}");
                        }
                    } else {
                        eprintln!("No SFTP session for host: {host}");
                    }
                    io_result = IOResult::CompletedRemote(host);
                }
                IOTask::CopyRemoteToLocalAndOpen {
                    host,
                    remote_path,
                    local_path,
                } => {
                    if let Some(session) = sftp_sessions.lock().unwrap().get(&host).cloned() {
                        let locked = session.lock().unwrap();
                        match crate::sftp::copy_remote_to_local_progress(
                            &locked.sftp,
                            &remote_path,
                            &local_path,
                            Some(&transfer_progress),
                        ) {
                            Ok(()) => open_with_default_app_bg(&local_path),
                            Err(e) => eprintln!("Remote-to-local copy error: {e}"),
                        }
                    } else {
                        eprintln!("No SFTP session for host: {host}");
                    }
                    io_result = IOResult::CompletedSilent;
                }
                IOTask::CopyRemoteSameHost {
                    host,
                    src_path,
                    dst_dir,
                    name,
                } => {
                    if let Some(session) = sftp_sessions.lock().unwrap().get(&host).cloned() {
                        let locked = session.lock().unwrap();
                        if let Err(e) = crate::sftp::recursive_copy_remote(
                            &locked.sftp,
                            &src_path,
                            &dst_dir,
                            &name,
                        ) {
                            eprintln!("Remote copy error: {e}");
                        }
                    } else {
                        eprintln!("No SFTP session for host: {host}");
                    }
                    io_result = IOResult::CompletedRemote(host);
                }
                IOTask::MoveRemoteSameHost {
                    host,
                    src_path,
                    dst_dir,
                    name,
                } => {
                    if let Some(session) = sftp_sessions.lock().unwrap().get(&host).cloned() {
                        let locked = session.lock().unwrap();
                        let dst_path = format!("{}/{}", dst_dir.trim_end_matches('/'), name);
                        if let Err(e) = crate::sftp::rename(&locked.sftp, &src_path, &dst_path) {
                            eprintln!("Remote move error: {e}");
                        }
                    } else {
                        eprintln!("No SFTP session for host: {host}");
                    }
                    io_result = IOResult::CompletedRemote(host);
                }
                IOTask::CopyContainerAndOpen {
                    kind,
                    archive_path,
                    inner_path,
                    dst_dir,
                    display_name,
                } => {
                    match copy_container_entry(
                        kind,
                        &archive_path,
                        &inner_path,
                        &dst_dir,
                        &display_name,
                    ) {
                        Ok(()) => open_with_default_app_bg(&dst_dir.join(&display_name)),
                        Err(e) => eprintln!("Extract error: {e}"),
                    }
                    io_result = IOResult::CompletedSilent;
                }
                IOTask::CopyRemoteCrossHost {
                    src_host,
                    src_path,
                    dst_host,
                    dst_dir,
                    name,
                    is_dir: _,
                } => {
                    let sessions = sftp_sessions.lock().unwrap();
                    let src_session = sessions.get(&src_host).cloned();
                    let dst_session = sessions.get(&dst_host).cloned();
                    drop(sessions);
                    match (src_session, dst_session) {
                        (Some(src_arc), Some(dst_arc)) => {
                            let src_locked = src_arc.lock().unwrap();
                            let dst_locked = dst_arc.lock().unwrap();
                            let total =
                                crate::sftp::count_bytes_via_exec(&src_locked.session, &src_path);
                            transfer_progress.reset(total);
                            if let Err(e) = crate::sftp::copy_cross_host_via_tar(
                                &src_locked.session,
                                &src_path,
                                &dst_locked.session,
                                &dst_dir,
                                &name,
                                &cancel_flag,
                                Some(&transfer_progress),
                            ) && e != "Cancelled"
                            {
                                eprintln!("Cross-host copy error: {e}");
                            }
                        }
                        (None, _) => eprintln!("No SFTP session for host: {src_host}"),
                        (_, None) => eprintln!("No SFTP session for host: {dst_host}"),
                    }
                    io_result = IOResult::CompletedRemote(dst_host);
                }
            }
            let _ = result_tx.send(io_result);
            if let Some(ref wake) = wake {
                wake();
            }
        }
    });
    (tx, result_rx, cancel_tx)
}

fn open_with_default_app_bg(path: &Path) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        // Use ShellExecuteW directly — no process spawning, no console flash.
        // `cmd /C start` spawns a console-subsystem process which causes a
        // brief terminal window to appear when the parent has no console.
        use std::os::windows::ffi::OsStrExt as _;
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn ShellExecuteW(
                hwnd: *mut std::ffi::c_void,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show_cmd: i32,
            ) -> isize;
        }
        let file: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let verb: Vec<u16> = std::ffi::OsStr::new("open")
            .encode_wide()
            .chain(Some(0))
            .collect();
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1, // SW_SHOWNORMAL
            );
        }
    }
}

#[cfg(unix)]
fn apply_props(path: &Path, mode: u32, uid: u32, gid: u32) -> std::io::Result<()> {
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions)?;
    let res = unsafe { libc::chown(path.as_os_str().as_bytes().as_ptr().cast(), uid, gid) };
    if res != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn apply_props_recursive(path: &Path, mode: u32, uid: u32, gid: u32) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    apply_props(path, mode, uid, gid)?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            apply_props_recursive(&entry.path(), mode, uid, gid)?;
        }
    }
    Ok(())
}

pub fn start_preview_worker(
    wake: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    sftp_sessions: Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<SftpSession>>>>>,
    transfer_progress: Arc<crate::core::TransferProgress>,
) -> (
    mpsc::Sender<PreviewRequest>,
    mpsc::Receiver<(u64, PreviewContent)>,
) {
    let (tx, rx) = mpsc::channel::<PreviewRequest>();
    let (result_tx, result_rx) = mpsc::channel::<(u64, PreviewContent)>();
    let current_id = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    thread::spawn(move || {
        while let Ok(request) = rx.recv() {
            current_id.store(
                preview_request_id(&request),
                std::sync::atomic::Ordering::Relaxed,
            );
            let current_id = std::sync::Arc::clone(&current_id);
            let wake = wake.clone();
            match request {
                PreviewRequest::Read {
                    id,
                    location,
                    max_bytes,
                } => {
                    let result_tx = result_tx.clone();
                    let sftp_sessions = sftp_sessions.clone();
                    let progress = transfer_progress.clone();
                    thread::spawn(move || match location {
                        EntryLocation::Fs(path) => {
                            let force_text = is_text_path(&path);
                            let file = File::open(&path);
                            if let Ok(file) = file {
                                let reader = std::io::BufReader::new(file);
                                if let Err(err) = send_streaming_preview(
                                    &result_tx,
                                    &current_id,
                                    id,
                                    reader,
                                    max_bytes,
                                    force_text,
                                    wake.as_ref(),
                                    None,
                                ) {
                                    let _ = result_tx.send((
                                        id,
                                        PreviewContent::Text(format!("Failed to read file: {err}")),
                                    ));
                                }
                            } else if let Err(err) = file {
                                let _ = result_tx.send((
                                    id,
                                    PreviewContent::Text(format!("Failed to read file: {err}")),
                                ));
                            }
                        }
                        EntryLocation::Container {
                            kind,
                            archive_path,
                            inner_path,
                        } => {
                            let force_text = is_text_name(&inner_path);
                            if let Err(err) = stream_container_preview(
                                &result_tx,
                                &current_id,
                                id,
                                kind,
                                &archive_path,
                                &inner_path,
                                max_bytes,
                                force_text,
                                wake.as_ref(),
                                Some(&progress),
                            ) {
                                let _ = result_tx.send((
                                    id,
                                    PreviewContent::Text(format!(
                                        "Failed to read archive entry: {err}"
                                    )),
                                ));
                            }
                        }
                        EntryLocation::Remote { host, path } => {
                            let force_text = is_text_name(&path);
                            let session = sftp_sessions.lock().unwrap().get(&host).cloned();
                            if let Some(session) = session {
                                let locked = session.lock().unwrap();
                                match crate::sftp::open_remote_reader(&locked.sftp, &path) {
                                    Ok(reader) => {
                                        if let Err(err) = send_streaming_preview(
                                            &result_tx,
                                            &current_id,
                                            id,
                                            reader,
                                            max_bytes,
                                            force_text,
                                            wake.as_ref(),
                                            Some(&progress),
                                        ) {
                                            let _ = result_tx.send((
                                                id,
                                                PreviewContent::Text(format!(
                                                    "Failed to read remote file: {err}"
                                                )),
                                            ));
                                        }
                                    }
                                    Err(err) => {
                                        let _ = result_tx.send((
                                            id,
                                            PreviewContent::Text(format!(
                                                "Failed to open remote file: {err}"
                                            )),
                                        ));
                                    }
                                }
                            } else {
                                let _ = result_tx.send((
                                    id,
                                    PreviewContent::Text(format!(
                                        "No SFTP session for host: {host}"
                                    )),
                                ));
                            }
                        }
                    });
                }
                PreviewRequest::ListContainer {
                    id,
                    kind,
                    archive_path,
                    max_entries,
                } => {
                    let entries = match read_container_directory(kind, &archive_path, "") {
                        Ok(entries) => entries,
                        Err(e) => {
                            let content =
                                PreviewContent::Text(format!("Failed to read archive: {e}"));
                            let _ = result_tx.send((id, content));
                            continue;
                        }
                    };
                    let listing =
                        format_container_listing(kind, &archive_path, &entries, max_entries);
                    let _ = result_tx.send((id, PreviewContent::Text(listing)));
                }
            }
        }
    });
    (tx, result_rx)
}

fn preview_request_id(request: &PreviewRequest) -> u64 {
    match *request {
        PreviewRequest::Read { id, .. } => id,
        PreviewRequest::ListContainer { id, .. } => id,
    }
}

pub fn start_dir_size_worker(
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (mpsc::Sender<PathBuf>, mpsc::Receiver<(PathBuf, u64)>) {
    let (tx, rx) = mpsc::channel::<PathBuf>();
    let (result_tx, result_rx) = mpsc::channel::<(PathBuf, u64)>();
    thread::spawn(move || {
        while let Ok(path) = rx.recv() {
            let size = compute_dir_size(&path);
            let _ = result_tx.send((path, size));
            if let Some(ref wake) = wake {
                wake();
            }
        }
    });
    (tx, result_rx)
}

fn is_preview_current(current_id: &std::sync::atomic::AtomicU64, id: u64) -> bool {
    current_id.load(std::sync::atomic::Ordering::Relaxed) == id
}

#[allow(clippy::too_many_arguments)]
fn send_streaming_preview<R: Read>(
    tx: &mpsc::Sender<(u64, PreviewContent)>,
    current_id: &std::sync::atomic::AtomicU64,
    id: u64,
    mut reader: R,
    max_bytes: Option<usize>,
    force_text: bool,
    wake: Option<&std::sync::Arc<dyn Fn() + Send + Sync>>,
    progress: Option<&crate::core::TransferProgress>,
) -> Result<(), std::io::Error> {
    let mut remaining = max_bytes.unwrap_or(usize::MAX);
    let mut buf = vec![0u8; PREVIEW_CHUNK_BYTES];
    let mut decided = force_text;
    let mut is_text = force_text;
    let mut bom_stripped = false;

    while remaining > 0 {
        if !is_preview_current(current_id, id) {
            return Ok(());
        }
        let to_read = buf.len().min(remaining);
        let read = reader.read(&mut buf[..to_read])?;
        if read == 0 {
            break;
        }
        remaining = remaining.saturating_sub(read);
        if let Some(p) = progress {
            p.add(read as u64);
        }
        let chunk = &buf[..read];
        if !decided {
            is_text = is_probably_text(chunk);
            decided = true;
        }
        if is_text {
            // Strip UTF-8 BOM from the first chunk
            let chunk = if !bom_stripped {
                bom_stripped = true;
                chunk.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(chunk)
            } else {
                chunk
            };
            let text = String::from_utf8_lossy(chunk).into_owned();
            let _ = tx.send((
                id,
                PreviewContent::TextChunk {
                    text,
                    done: remaining == 0,
                },
            ));
        } else {
            let _ = tx.send((
                id,
                PreviewContent::BinaryChunk {
                    data: chunk.to_vec(),
                    done: remaining == 0,
                },
            ));
        }
        if let Some(wake) = wake {
            wake();
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn stream_container_preview(
    tx: &mpsc::Sender<(u64, PreviewContent)>,
    current_id: &std::sync::atomic::AtomicU64,
    id: u64,
    kind: crate::core::ContainerKind,
    archive_path: &Path,
    inner_path: &str,
    max_bytes: Option<usize>,
    force_text: bool,
    wake: Option<&std::sync::Arc<dyn Fn() + Send + Sync>>,
    progress: Option<&crate::core::TransferProgress>,
) -> Result<(), String> {
    let normalized = inner_path.trim_start_matches('/');
    let file = File::open(archive_path).map_err(|e| e.to_string())?;
    if let Some(p) = progress {
        let total = file.metadata().map(|m| m.len()).unwrap_or(0);
        p.reset(total);
    }
    match kind {
        crate::core::ContainerKind::Zip => {
            let reader = std::io::BufReader::new(file);
            let mut zip = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
            for i in 0..zip.len() {
                if !is_preview_current(current_id, id) {
                    return Ok(());
                }
                let entry = zip.by_index(i).map_err(|e| e.to_string())?;
                if entry.name() == normalized {
                    return send_streaming_preview(
                        tx, current_id, id, entry, max_bytes, force_text, wake, progress,
                    )
                    .map_err(|e| e.to_string());
                }
            }
        }
        crate::core::ContainerKind::Tar => {
            let reader = std::io::BufReader::new(file);
            let mut archive = tar::Archive::new(reader);
            for entry in archive.entries().map_err(|e| e.to_string())? {
                if !is_preview_current(current_id, id) {
                    return Ok(());
                }
                let mut entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path().map_err(|e| e.to_string())?;
                let name = crate::core::normalize_archive_path(&path);
                if name == normalized {
                    return send_streaming_preview(
                        tx, current_id, id, &mut entry, max_bytes, force_text, wake, progress,
                    )
                    .map_err(|e| e.to_string());
                }
            }
        }
        crate::core::ContainerKind::TarGz => {
            let reader = std::io::BufReader::new(file);
            let decoder = flate2::read::GzDecoder::new(reader);
            let mut archive = tar::Archive::new(decoder);
            for entry in archive.entries().map_err(|e| e.to_string())? {
                if !is_preview_current(current_id, id) {
                    return Ok(());
                }
                let mut entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path().map_err(|e| e.to_string())?;
                let name = crate::core::normalize_archive_path(&path);
                if name == normalized {
                    return send_streaming_preview(
                        tx, current_id, id, &mut entry, max_bytes, force_text, wake, progress,
                    )
                    .map_err(|e| e.to_string());
                }
            }
        }
        crate::core::ContainerKind::TarBz2 => {
            let reader = std::io::BufReader::new(file);
            let decoder = bzip2::read::BzDecoder::new(reader);
            let mut archive = tar::Archive::new(decoder);
            for entry in archive.entries().map_err(|e| e.to_string())? {
                if !is_preview_current(current_id, id) {
                    return Ok(());
                }
                let mut entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path().map_err(|e| e.to_string())?;
                let name = crate::core::normalize_archive_path(&path);
                if name == normalized {
                    return send_streaming_preview(
                        tx, current_id, id, &mut entry, max_bytes, force_text, wake, progress,
                    )
                    .map_err(|e| e.to_string());
                }
            }
        }
    }
    Err(format!("Entry not found in archive: {inner_path}"))
}

pub fn start_search_worker(
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (mpsc::Sender<SearchRequest>, mpsc::Receiver<SearchEvent>) {
    let (tx, rx) = mpsc::channel::<SearchRequest>();
    let (result_tx, result_rx) = mpsc::channel::<SearchEvent>();
    thread::spawn(move || {
        let mut pending: Option<SearchRequest> = None;
        'worker: loop {
            let request = match pending.take() {
                Some(request) => request,
                None => match rx.recv() {
                    Ok(request) => request,
                    Err(_) => break,
                },
            };
            let mut progress = SearchProgress {
                scanned: 0,
                matched: 0,
            };
            let mut stack = vec![request.root.clone()];
            let mut needle = request.needle.clone();
            if request.case == SearchCase::Insensitive {
                needle = needle.to_ascii_lowercase();
            }
            let use_wildcard = needle.contains('*') || needle.contains('?');
            let mut tick = 0usize;

            loop {
                if let Ok(new_request) = rx.try_recv() {
                    pending = Some(new_request);
                    continue 'worker;
                }
                let dir = match stack.pop() {
                    Some(dir) => dir,
                    None => {
                        let _ = result_tx.send(SearchEvent::Done {
                            id: request.id,
                            progress,
                        });
                        if let Some(ref wake) = wake {
                            wake();
                        }
                        continue 'worker;
                    }
                };
                let read_dir = match std::fs::read_dir(&dir) {
                    Ok(rd) => rd,
                    Err(_) => continue,
                };
                for entry in read_dir.flatten() {
                    tick = tick.wrapping_add(1);
                    if tick.is_multiple_of(256) {
                        if let Ok(new_request) = rx.try_recv() {
                            pending = Some(new_request);
                            continue 'worker;
                        }
                        let _ = result_tx.send(SearchEvent::Progress {
                            id: request.id,
                            progress,
                        });
                        if let Some(ref wake) = wake {
                            wake();
                        }
                    }
                    progress.scanned = progress.scanned.saturating_add(1);
                    let path = entry.path();
                    let file_type = match entry.file_type() {
                        Ok(ft) => ft,
                        Err(_) => continue,
                    };
                    let metadata = entry.metadata().ok();
                    let modified = metadata
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    match request.mode {
                        SearchMode::Name => {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let haystack = if request.case == SearchCase::Insensitive {
                                name.to_ascii_lowercase()
                            } else {
                                name.clone()
                            };
                            let matched = if use_wildcard {
                                wildcard_match(&haystack, &needle)
                            } else {
                                haystack.contains(&needle)
                            };
                            if matched {
                                let size = if file_type.is_file() {
                                    metadata.as_ref().map(|m| m.len())
                                } else {
                                    None
                                };
                                let _ = result_tx.send(SearchEvent::Match {
                                    id: request.id,
                                    result: SearchResult {
                                        path: path.clone(),
                                        is_dir: file_type.is_dir(),
                                        size,
                                        modified,
                                    },
                                });
                                progress.matched = progress.matched.saturating_add(1);
                            }
                            if file_type.is_dir() {
                                stack.push(path);
                            }
                        }
                        SearchMode::Content => {
                            if file_type.is_dir() {
                                stack.push(path);
                                continue;
                            }
                            if !file_type.is_file() {
                                continue;
                            }
                            if file_contains(&path, &needle, request.case).unwrap_or(false) {
                                let size = metadata.as_ref().map(|m| m.len());
                                let _ = result_tx.send(SearchEvent::Match {
                                    id: request.id,
                                    result: SearchResult {
                                        path: path.clone(),
                                        is_dir: false,
                                        size,
                                        modified,
                                    },
                                });
                                progress.matched = progress.matched.saturating_add(1);
                            }
                        }
                    }
                }
            }
        }
    });
    (tx, result_rx)
}

fn file_contains(path: &PathBuf, needle: &str, case: SearchCase) -> std::io::Result<bool> {
    if needle.is_empty() {
        return Ok(false);
    }
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut carry: Vec<u8> = Vec::new();
    let needle_bytes = needle.as_bytes();
    let needle_lower = if case == SearchCase::Insensitive {
        Some(needle.to_ascii_lowercase().into_bytes())
    } else {
        None
    };
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        let mut window = Vec::with_capacity(carry.len() + read);
        if !carry.is_empty() {
            window.extend_from_slice(&carry);
        }
        window.extend_from_slice(&buf[..read]);

        let found = if let Some(needle_lower) = needle_lower.as_ref() {
            let mut lowered = window.clone();
            for byte in &mut lowered {
                *byte = byte.to_ascii_lowercase();
            }
            memchr::memmem::find(&lowered, needle_lower).is_some()
        } else {
            memchr::memmem::find(&window, needle_bytes).is_some()
        };
        if found {
            return Ok(true);
        }

        let keep = needle_bytes.len().saturating_sub(1);
        if keep > 0 {
            if window.len() >= keep {
                carry = window[window.len() - keep..].to_vec();
            } else {
                carry = window;
            }
        } else {
            carry.clear();
        }
    }
    Ok(false)
}

fn wildcard_match(text: &str, pattern: &str) -> bool {
    let mut t = 0usize;
    let mut p = 0usize;
    let mut star_idx: Option<usize> = None;
    let mut match_idx = 0usize;
    let text_bytes = text.as_bytes();
    let pat_bytes = pattern.as_bytes();

    while t < text_bytes.len() {
        if p < pat_bytes.len() && (pat_bytes[p] == b'?' || pat_bytes[p] == text_bytes[t]) {
            p += 1;
            t += 1;
        } else if p < pat_bytes.len() && pat_bytes[p] == b'*' {
            star_idx = Some(p);
            match_idx = t;
            p += 1;
        } else if let Some(star) = star_idx {
            p = star + 1;
            match_idx += 1;
            t = match_idx;
        } else {
            return false;
        }
    }
    while p < pat_bytes.len() && pat_bytes[p] == b'*' {
        p += 1;
    }
    p == pat_bytes.len()
}

fn compute_dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file()
                && let Ok(meta) = entry.metadata()
            {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}
