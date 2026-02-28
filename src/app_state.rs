use std::{
    collections::{HashMap, HashSet},
    path,
    sync::mpsc,
};

use crate::core::{
    ActivePanel, ContainerKind, DirBatch, DirEntry, EntryLocation, IOResult, IOTask, ImageLocation,
    PanelMode, PreviewContent, PreviewRequest, container_display_path, container_kind_from_path,
    format_preview_info, is_image_name, is_image_path, is_text_name, is_text_path,
};
use crate::theme::Theme;

pub struct PanelState {
    pub current_path: path::PathBuf, // For Fs mode: real fs path. For Container: archive path.
    pub mode: PanelMode,
    pub selected_index: usize,
    pub entries: Vec<DirEntry>,
    pub entries_rx: Option<mpsc::Receiver<DirBatch>>,
    pub prefer_select_name: Option<String>,
    pub top_index: usize,
    pub loading: bool,
    pub loading_progress: Option<(usize, Option<usize>)>,
    pub dir_token: u64,
}

pub struct AppState {
    pub left_panel: PanelState,
    pub right_panel: PanelState,
    pub active_panel: ActivePanel,
    pub preview: Option<PreviewContent>,
    pub preview_key: Option<String>,
    pub preview_ext: Option<String>,
    pub preview_tx: mpsc::Sender<PreviewRequest>,
    pub preview_rx: mpsc::Receiver<(u64, PreviewContent)>,
    pub preview_request_id: u64,
    pub io_tx: mpsc::Sender<IOTask>,
    pub io_rx: mpsc::Receiver<IOResult>,
    pub io_cancel_tx: mpsc::Sender<()>,
    pub io_in_flight: usize,
    pub io_cancel_requested: bool,
    pub dir_size_tx: mpsc::Sender<path::PathBuf>,
    pub dir_size_rx: mpsc::Receiver<(path::PathBuf, u64)>,
    pub dir_sizes: HashMap<path::PathBuf, u64>,
    pub dir_size_pending: HashSet<path::PathBuf>,
    pub fs_last_selected_name: HashMap<path::PathBuf, String>,
    pub container_last_selected_name: HashMap<(path::PathBuf, String, ContainerKind), String>,
    pub theme: Theme,
    pub theme_picker_open: bool,
    pub theme_picker_selected: Option<usize>,
    pub pending_op: Option<PendingOp>,
    pub rename_input: Option<String>,
    pub rename_focus: bool,
}

#[derive(Clone)]
pub enum PendingOp {
    Copy {
        src: EntryLocation,
        dst_dir: path::PathBuf,
        kind: CopyKind,
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
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CopyKind {
    File,
    Directory,
}

impl AppState {
    pub fn panel(&self, which: ActivePanel) -> &PanelState {
        match which {
            ActivePanel::Left => &self.left_panel,
            ActivePanel::Right => &self.right_panel,
        }
    }

    pub fn panel_mut(&mut self, which: ActivePanel) -> &mut PanelState {
        match which {
            ActivePanel::Left => &mut self.left_panel,
            ActivePanel::Right => &mut self.right_panel,
        }
    }

    pub fn get_active_panel(&self) -> &PanelState {
        self.panel(self.active_panel.clone())
    }

    pub fn get_active_panel_mut(&mut self) -> &mut PanelState {
        self.panel_mut(self.active_panel.clone())
    }

    pub fn select_entry(&mut self, index: usize, window_rows: usize) {
        let panel = self.get_active_panel_mut();
        if index < panel.entries.len() {
            panel.selected_index = index;
            if panel.selected_index < panel.top_index {
                panel.top_index = panel.selected_index;
            } else if panel.selected_index >= panel.top_index + window_rows {
                panel.top_index = panel.selected_index + 1 - window_rows;
            }
            if self.preview.is_some() {
                self.update_preview_for_current_selection();
            }
        } else {
            log::error!("Unable to select entry at index {}", index);
        }
    }

    pub fn switch_panel(&mut self) {
        self.active_panel = match self.active_panel {
            ActivePanel::Left => ActivePanel::Right,
            ActivePanel::Right => ActivePanel::Left,
        };
    }

    pub fn store_current_selection_memory(&mut self) {
        self.store_selection_memory_for(self.active_panel.clone());
    }

    pub fn store_selection_memory_for(&mut self, which: ActivePanel) {
        let (fs_key, container_key, selected_name_opt) = {
            let panel = self.panel(which);
            if panel.entries.is_empty() {
                return;
            }
            let selected_name = panel.entries[panel.selected_index].name.clone();
            match &panel.mode {
                PanelMode::Fs => (Some(panel.current_path.clone()), None, Some(selected_name)),
                PanelMode::Container {
                    archive_path,
                    cwd,
                    kind,
                } => (
                    None,
                    Some((archive_path.clone(), cwd.clone(), *kind)),
                    Some(selected_name),
                ),
            }
        };
        if let Some(selected_name) = selected_name_opt {
            if let Some(path) = fs_key {
                self.fs_last_selected_name.insert(path, selected_name);
            } else if let Some((ap, cwd, kind)) = container_key {
                self.container_last_selected_name
                    .insert((ap, cwd, kind), selected_name);
            }
        }
    }

    pub fn select_entry_by_name(&mut self, which: ActivePanel, name: &str) {
        let panel = self.panel_mut(which);
        if let Some(idx) = panel.entries.iter().position(|e| e.name == name) {
            panel.selected_index = idx;
        }
    }

    pub fn prepare_rename_selected(&mut self) {
        let (path, name) = {
            let panel = self.get_active_panel();
            if panel.entries.is_empty() {
                return;
            }
            let entry = &panel.entries[panel.selected_index];
            if entry.name == ".." || entry.is_dir && !matches!(entry.location, EntryLocation::Fs(_))
            {
                return;
            }
            if let EntryLocation::Fs(path) = &entry.location {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string());
                (Some(path.clone()), name)
            } else {
                (None, None)
            }
        };
        if let (Some(path), Some(name)) = (path, name) {
            self.rename_input = Some(name);
            self.pending_op = Some(PendingOp::Rename { src: path });
            self.rename_focus = true;
        }
    }

    pub fn update_preview_for_current_selection(&mut self) {
        let (is_dir, location, key, ext) = {
            let panel = self.get_active_panel();
            if panel.entries.is_empty() {
                self.clear_preview();
                return;
            }
            let entry = &panel.entries[panel.selected_index];
            let ext = std::path::Path::new(&entry.name)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_string());
            let key = match &entry.location {
                EntryLocation::Fs(path) => path.to_string_lossy().into_owned(),
                EntryLocation::Container {
                    kind,
                    archive_path,
                    inner_path,
                } => container_display_path(*kind, archive_path, inner_path),
            };
            (entry.is_dir, entry.location.clone(), key, ext)
        };
        self.preview_key = Some(key);
        self.preview_ext = ext;
        self.preview_request_id = self.preview_request_id.wrapping_add(1);
        let request_id = self.preview_request_id;
        const MAX_BYTES_TEXT: usize = 64 * 1024;
        const MAX_BYTES_BINARY: usize = 4 * 1024;
        if is_dir {
            self.preview = Some(PreviewContent::Text(format_preview_info(
                "Directory",
                &location,
            )));
            return;
        }
        match location {
            EntryLocation::Fs(path) => {
                if is_image_path(&path) {
                    self.preview = Some(PreviewContent::Image(ImageLocation::Fs(
                        std::sync::Arc::from(path),
                    )));
                    return;
                }
                if let Some(kind) = container_kind_from_path(&path) {
                    self.preview_request_id = self.preview_request_id.wrapping_add(1);
                    let request_id = self.preview_request_id;
                    self.preview = Some(PreviewContent::Text(format_preview_info(
                        "Archive",
                        &EntryLocation::Container {
                            kind,
                            archive_path: path.clone(),
                            inner_path: String::new(),
                        },
                    )));
                    let _ = self.preview_tx.send(PreviewRequest::ListContainer {
                        id: request_id,
                        kind,
                        archive_path: path,
                        max_entries: 200,
                    });
                    return;
                }
                if self.preview.is_none() {
                    self.preview = Some(PreviewContent::Text(format_preview_info(
                        "File",
                        &EntryLocation::Fs(path.clone()),
                    )));
                }
                let max_bytes = if is_text_path(&path) {
                    MAX_BYTES_TEXT
                } else {
                    MAX_BYTES_BINARY
                };
                let _ = self.preview_tx.send(PreviewRequest::Read {
                    id: request_id,
                    location: EntryLocation::Fs(path),
                    max_bytes,
                });
            }
            EntryLocation::Container {
                kind,
                archive_path,
                inner_path,
            } => {
                if is_image_name(&inner_path) {
                    self.preview = Some(PreviewContent::Image(ImageLocation::Container {
                        kind,
                        archive_path: archive_path.clone(),
                        inner_path: inner_path.clone(),
                    }));
                    return;
                }
                if self.preview.is_none() {
                    self.preview = Some(PreviewContent::Text(format_preview_info(
                        "File",
                        &EntryLocation::Container {
                            kind,
                            archive_path: archive_path.clone(),
                            inner_path: inner_path.clone(),
                        },
                    )));
                }
                let max_bytes = if is_text_name(&inner_path) {
                    MAX_BYTES_TEXT
                } else {
                    MAX_BYTES_BINARY
                };
                let _ = self.preview_tx.send(PreviewRequest::Read {
                    id: request_id,
                    location: EntryLocation::Container {
                        kind,
                        archive_path,
                        inner_path,
                    },
                    max_bytes,
                });
            }
        }
    }

    pub fn toggle_preview(&mut self) {
        if self.preview.is_some() {
            self.clear_preview();
            return;
        }
        self.update_preview_for_current_selection();
    }

    pub fn prepare_copy_selected(&mut self) {
        if self.pending_op.is_some() {
            return;
        }
        if let Some(op) = self.build_copy_op() {
            self.pending_op = Some(op);
        }
    }

    pub fn prepare_move_selected(&mut self) {
        if self.pending_op.is_some() {
            return;
        }
        if let Some(op) = self.build_move_op() {
            self.pending_op = Some(op);
        }
    }

    pub fn prepare_delete_selected(&mut self) {
        if self.pending_op.is_some() {
            return;
        }
        if let Some(op) = self.build_delete_op() {
            self.pending_op = Some(op);
        }
    }

    pub fn take_pending_op(&mut self) -> Option<PendingOp> {
        self.pending_op.take()
    }

    pub fn clear_pending_op(&mut self) {
        self.pending_op = None;
        self.rename_input = None;
        self.rename_focus = false;
    }

    pub fn enqueue_pending_op(&mut self, op: &PendingOp) {
        match op {
            PendingOp::Copy { src, dst_dir, kind } => {
                let task = match src {
                    EntryLocation::Fs(path) => IOTask::Copy {
                        src: path.clone(),
                        dst_dir: dst_dir.clone(),
                    },
                    EntryLocation::Container {
                        kind: container_kind,
                        archive_path,
                        inner_path,
                    } => match kind {
                        CopyKind::File => IOTask::CopyContainer {
                            kind: *container_kind,
                            archive_path: archive_path.clone(),
                            inner_path: inner_path.clone(),
                            dst_dir: dst_dir.clone(),
                            display_name: src.display_name(),
                        },
                        CopyKind::Directory => IOTask::CopyContainerDir {
                            kind: *container_kind,
                            archive_path: archive_path.clone(),
                            inner_path: inner_path.clone(),
                            dst_dir: dst_dir.clone(),
                            display_name: src.display_name(),
                        },
                    },
                };
                if let Err(e) = self.io_tx.send(task) {
                    eprintln!("Failed to enqueue copy: {e}");
                } else {
                    self.io_in_flight = self.io_in_flight.saturating_add(1);
                }
            }
            PendingOp::Move { src, dst_dir } => {
                if let Err(e) = self.io_tx.send(IOTask::Move {
                    src: src.clone(),
                    dst_dir: dst_dir.clone(),
                }) {
                    eprintln!("Failed to enqueue move: {e}");
                } else {
                    self.io_in_flight = self.io_in_flight.saturating_add(1);
                    log::info!(
                        "Enqueued move: {} -> {}",
                        src.to_string_lossy(),
                        dst_dir.to_string_lossy()
                    );
                }
            }
            PendingOp::Delete { target } => {
                if let Err(e) = self.io_tx.send(IOTask::Delete {
                    target: target.clone(),
                }) {
                    eprintln!("Failed to enqueue delete: {e}");
                } else {
                    self.io_in_flight = self.io_in_flight.saturating_add(1);
                    log::info!("Enqueued delete: {}", target.to_string_lossy());
                }
            }
            PendingOp::Rename { src } => {
                if let Some(new_name) = self.rename_input.clone() {
                    if let Err(e) = self.io_tx.send(IOTask::Rename {
                        src: src.clone(),
                        new_name,
                    }) {
                        eprintln!("Failed to enqueue rename: {e}");
                    } else {
                        self.io_in_flight = self.io_in_flight.saturating_add(1);
                        log::info!("Enqueued rename: {}", src.to_string_lossy());
                    }
                }
            }
        }
    }

    pub fn on_io_completed(&mut self, count: usize) {
        self.io_in_flight = self.io_in_flight.saturating_sub(count);
        if self.io_in_flight == 0 {
            self.io_cancel_requested = false;
        }
    }

    pub fn request_io_cancel(&mut self) {
        if self.io_in_flight == 0 {
            return;
        }
        self.io_cancel_requested = true;
        let _ = self.io_cancel_tx.send(());
    }

    fn build_copy_op(&self) -> Option<PendingOp> {
        let (src, kind) = {
            let p = self.get_active_panel();
            if p.entries.is_empty() {
                return None;
            }
            let entry = &p.entries[p.selected_index];
            let kind = if entry.is_dir {
                CopyKind::Directory
            } else {
                CopyKind::File
            };
            (entry.location.clone(), kind)
        };

        let dst_dir = {
            let other_panel = match self.active_panel {
                ActivePanel::Left => &self.right_panel,
                ActivePanel::Right => &self.left_panel,
            };
            match &other_panel.mode {
                PanelMode::Fs => other_panel.current_path.clone(),
                PanelMode::Container { .. } => {
                    return None;
                }
            }
        };

        Some(PendingOp::Copy { src, dst_dir, kind })
    }

    fn build_move_op(&self) -> Option<PendingOp> {
        let src = {
            let p = self.get_active_panel();
            if p.entries.is_empty() {
                return None;
            }
            match &p.entries[p.selected_index].location {
                EntryLocation::Fs(path) => path.clone(),
                EntryLocation::Container { .. } => {
                    return None;
                }
            }
        };

        let dst_dir = {
            let other_panel = match self.active_panel {
                ActivePanel::Left => &self.right_panel,
                ActivePanel::Right => &self.left_panel,
            };
            match &other_panel.mode {
                PanelMode::Fs => other_panel.current_path.clone(),
                PanelMode::Container { .. } => {
                    return None;
                }
            }
        };

        Some(PendingOp::Move { src, dst_dir })
    }

    fn build_delete_op(&self) -> Option<PendingOp> {
        let target = {
            let p = self.get_active_panel();
            if p.entries.is_empty() {
                return None;
            }
            let entry = &p.entries[p.selected_index];
            if entry.name == ".." {
                return None;
            }
            match &entry.location {
                EntryLocation::Fs(path) => path.clone(),
                EntryLocation::Container { .. } => {
                    return None;
                }
            }
        };

        Some(PendingOp::Delete { target })
    }

    pub fn switch_theme(&mut self) {
        if self.theme.selected_external.is_some() && self.theme_picker_open {
            self.apply_selected_theme();
        } else {
            self.theme.toggle();
        }
    }

    pub fn open_theme_picker(&mut self) {
        self.theme_picker_open = true;
        self.theme_picker_selected = self.theme.selected_external.or(Some(0));
    }

    pub fn close_theme_picker(&mut self) {
        self.theme_picker_open = false;
    }

    pub fn select_next_theme(&mut self) {
        if self.theme.external.is_empty() {
            return;
        }
        let len = self.theme.external.len();
        let cur = self.theme_picker_selected.unwrap_or(0);
        self.theme_picker_selected = Some((cur + 1) % len);
    }

    pub fn select_prev_theme(&mut self) {
        if self.theme.external.is_empty() {
            return;
        }
        let len = self.theme.external.len();
        let cur = self.theme_picker_selected.unwrap_or(0);
        self.theme_picker_selected = Some((cur + len - 1) % len);
    }

    pub fn apply_selected_theme(&mut self) {
        if let Some(i) = self.theme_picker_selected
            && i < self.theme.external.len()
        {
            self.theme.selected_external = Some(i);
        }
        self.theme_picker_open = false;
    }

    pub fn theme_names(&self) -> Vec<String> {
        if self.theme.external.is_empty() {
            vec!["Dark".to_string(), "Light".to_string()]
        } else {
            self.theme.external.iter().map(|(n, _)| n.clone()).collect()
        }
    }

    pub fn clear_preview(&mut self) {
        self.preview = None;
        self.preview_key = None;
        self.preview_ext = None;
        self.preview_request_id = self.preview_request_id.wrapping_add(1);
    }
}
