use std::{fs::File, io::Read, path::PathBuf, sync::mpsc, thread};

use crate::core::{
    EntryLocation, IOResult, IOTask, PreviewContent, PreviewRequest, SearchCase, SearchEvent,
    SearchMode, SearchProgress, SearchRequest, SearchResult, copy_container_dir,
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
                IOTask::Rename { src, new_name } => {
                    let target = src.with_file_name(new_name);
                    if let Err(e) = std::fs::rename(&src, &target) {
                        eprintln!("Rename error: {e}");
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

pub fn start_search_worker() -> (mpsc::Sender<SearchRequest>, mpsc::Receiver<SearchEvent>) {
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
                        continue 'worker;
                    }
                };
                let read_dir = match std::fs::read_dir(&dir) {
                    Ok(rd) => rd,
                    Err(_) => continue,
                };
                for entry in read_dir.flatten() {
                    tick = tick.wrapping_add(1);
                    if tick % 256 == 0 {
                        if let Ok(new_request) = rx.try_recv() {
                            pending = Some(new_request);
                            continue 'worker;
                        }
                        let _ = result_tx.send(SearchEvent::Progress {
                            id: request.id,
                            progress,
                        });
                    }
                    progress.scanned = progress.scanned.saturating_add(1);
                    let path = entry.path();
                    let file_type = match entry.file_type() {
                        Ok(ft) => ft,
                        Err(_) => continue,
                    };
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
                                    entry.metadata().ok().map(|m| m.len())
                                } else {
                                    None
                                };
                                let _ = result_tx.send(SearchEvent::Match {
                                    id: request.id,
                                    result: SearchResult {
                                        path: path.clone(),
                                        is_dir: file_type.is_dir(),
                                        size,
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
                                let size = entry.metadata().ok().map(|m| m.len());
                                let _ = result_tx.send(SearchEvent::Match {
                                    id: request.id,
                                    result: SearchResult {
                                        path: path.clone(),
                                        is_dir: false,
                                        size,
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
