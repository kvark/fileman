use std::{sync::mpsc, thread};

use crate::core::{
    EntryLocation, IOTask, PreviewContent, PreviewRequest, copy_recursively, hexdump,
    is_probably_text, read_bytes_prefix, read_zip_bytes_prefix,
};

pub fn start_io_worker() -> mpsc::Sender<IOTask> {
    let (tx, rx) = mpsc::channel::<IOTask>();
    thread::spawn(move || {
        while let Ok(task) = rx.recv() {
            match task {
                IOTask::Copy { src, dst_dir } => {
                    if let Err(e) = copy_recursively(&src, &dst_dir) {
                        eprintln!("Copy error: {e}");
                    }
                }
            }
        }
    });
    tx
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
                        EntryLocation::Zip {
                            archive_path,
                            inner_path,
                        } => match read_zip_bytes_prefix(&archive_path, &inner_path, max_bytes) {
                            Ok(bytes) => {
                                if is_probably_text(&bytes) {
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
            }
        }
    });
    (tx, result_rx)
}
