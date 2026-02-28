use std::{path::PathBuf, sync::mpsc, thread};

use crate::core::{
    EntryLocation, IOResult, IOTask, PreviewContent, PreviewRequest, copy_container_dir,
    copy_container_entry, copy_recursively, format_container_listing, hexdump, is_probably_text,
    is_text_name, read_bytes_prefix, read_container_bytes_prefix, read_container_directory,
};

pub fn start_io_worker() -> (
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
            }
            if cancel_requested {
                let _ = result_tx.send(IOResult::Completed);
                while let Ok(_dropped) = rx.try_recv() {
                    let _ = result_tx.send(IOResult::Completed);
                }
                cancel_requested = false;
                continue;
            }
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
            }
            let _ = result_tx.send(IOResult::Completed);
        }
    });
    (tx, result_rx, cancel_tx)
}

pub fn start_preview_worker() -> (
    mpsc::Sender<PreviewRequest>,
    mpsc::Receiver<(u64, PreviewContent)>,
) {
    let (tx, rx) = mpsc::channel::<PreviewRequest>();
    let (result_tx, result_rx) = mpsc::channel::<(u64, PreviewContent)>();
    thread::spawn(move || {
        while let Ok(mut request) = rx.recv() {
            while let Ok(next) = rx.try_recv() {
                request = next;
            }
            match request {
                PreviewRequest::Read {
                    id,
                    location,
                    max_bytes,
                } => {
                    let content = match location {
                        EntryLocation::Fs(path) => match read_bytes_prefix(&path, max_bytes) {
                            Ok(bytes) => {
                                if is_probably_text(&bytes) {
                                    let text = String::from_utf8_lossy(&bytes).into_owned();
                                    PreviewContent::Text(text)
                                } else {
                                    PreviewContent::Text(hexdump(&bytes))
                                }
                            }
                            Err(e) => PreviewContent::Text(format!("Failed to read file: {e}")),
                        },
                        EntryLocation::Container {
                            kind,
                            archive_path,
                            inner_path,
                        } => match read_container_bytes_prefix(
                            kind,
                            &archive_path,
                            &inner_path,
                            max_bytes,
                        ) {
                            Ok(bytes) => {
                                if is_text_name(&inner_path) || is_probably_text(&bytes) {
                                    let text = String::from_utf8_lossy(&bytes).into_owned();
                                    PreviewContent::Text(text)
                                } else {
                                    PreviewContent::Text(hexdump(&bytes))
                                }
                            }
                            Err(e) => {
                                PreviewContent::Text(format!("Failed to read zip entry: {e}"))
                            }
                        },
                    };
                    let _ = result_tx.send((id, content));
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

pub fn start_dir_size_worker() -> (mpsc::Sender<PathBuf>, mpsc::Receiver<(PathBuf, u64)>) {
    let (tx, rx) = mpsc::channel::<PathBuf>();
    let (result_tx, result_rx) = mpsc::channel::<(PathBuf, u64)>();
    thread::spawn(move || {
        while let Ok(path) = rx.recv() {
            let size = compute_dir_size(&path);
            let _ = result_tx.send((path, size));
        }
    });
    (tx, result_rx)
}

fn compute_dir_size(root: &PathBuf) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.clone()];
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
            } else if file_type.is_file() {
                if let Ok(meta) = entry.metadata() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
    }
    total
}
