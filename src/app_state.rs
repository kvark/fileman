use std::{
    collections::{HashMap, HashSet},
    path,
    sync::mpsc,
    time::Instant,
};

use crate::core::{
    ActivePanel, BrowserMode, ContainerKind, DirBatch, DirEntry, EditLoadRequest, EditLoadResult,
    EntryLocation, IOResult, IOTask, ImageLocation, PreviewContent, PreviewRequest, SearchCase,
    SearchMode, SearchResult, container_display_path, container_kind_from_path,
    format_preview_info, is_image_name, is_image_path, is_text_name, is_text_path,
};
use crate::theme::Theme;

pub struct PanelState {
    pub browser: BrowserState,
    pub mode: PanelMode,
}

pub enum PanelMode {
    Browser,
    Preview(PreviewState),
    Edit(EditState),
}

pub struct BrowserState {
    pub browser_mode: BrowserMode,
    pub current_path: path::PathBuf, // For Fs mode: real fs path. For Container: archive path.
    pub selected_index: usize,
    pub entries: Vec<DirEntry>,
    pub entries_rx: Option<mpsc::Receiver<DirBatch>>,
    pub prefer_select_name: Option<String>,
    pub top_index: usize,
    pub loading: bool,
    pub loading_progress: Option<(usize, Option<usize>)>,
    pub dir_token: u64,
    pub history_back: Vec<PanelSnapshot>,
    pub history_forward: Vec<PanelSnapshot>,
}

pub struct PreviewState {
    pub content: Option<PreviewContent>,
    pub key: Option<String>,
    pub ext: Option<String>,
    pub scroll: f32,
    pub line_height: f32,
    pub page_height: f32,
    pub can_scroll: bool,
    pub find_open: bool,
    pub find_query: String,
    pub find_index: usize,
    pub find_focus: bool,
    pub request_id: u64,
}

pub struct EditState {
    pub path: Option<path::PathBuf>,
    pub text: String,
    pub ext: Option<String>,
    pub loading: bool,
    pub dirty: bool,
    pub confirm_discard: bool,
    pub return_focus: ActivePanel,
    pub highlight_key: Option<String>,
    pub highlight_hash: u64,
    pub highlight_wrap_width: f32,
    pub highlight_dirty_at: Option<Instant>,
    pub request_id: u64,
}

#[derive(Clone)]
pub struct PanelSnapshot {
    pub mode: BrowserMode,
    pub current_path: path::PathBuf,
    pub selected_name: Option<String>,
}

fn history_key(snapshot: &PanelSnapshot) -> String {
    match &snapshot.mode {
        BrowserMode::Fs => format!("fs:{}", snapshot.current_path.to_string_lossy()),
        BrowserMode::Container {
            kind,
            archive_path,
            cwd,
        } => format!(
            "container:{}:{}:{}",
            match kind {
                ContainerKind::Zip => "zip",
                ContainerKind::TarGz => "tar.gz",
                ContainerKind::TarBz2 => "tar.bz2",
            },
            archive_path.to_string_lossy(),
            cwd
        ),
        BrowserMode::Search {
            root,
            query,
            mode,
            case,
        } => format!(
            "search:{}:{}:{}:{}",
            root.to_string_lossy(),
            query,
            match mode {
                SearchMode::Name => "name",
                SearchMode::Content => "content",
            },
            match case {
                SearchCase::Sensitive => "s",
                SearchCase::Insensitive => "i",
            }
        ),
    }
}

pub struct AppState {
    pub left_panel: PanelState,
    pub right_panel: PanelState,
    pub active_panel: ActivePanel,
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
    pub edit_tx: mpsc::Sender<EditLoadRequest>,
    pub edit_rx: mpsc::Receiver<EditLoadResult>,
    pub edit_request_id: u64,
    pub search_query: String,
    pub search_focus: bool,
    pub search_case: SearchCase,
    pub search_mode: SearchMode,
    pub search_results: Vec<SearchResult>,
    pub search_selected: usize,
    pub search_request_id: u64,
    pub search_status: SearchStatus,
    pub search_ui: SearchUiState,
    pub search_tx: mpsc::Sender<crate::core::SearchRequest>,
    pub search_rx: mpsc::Receiver<crate::core::SearchEvent>,
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchUiState {
    Closed,
    Open,
}

#[derive(Clone, Copy)]
pub enum SearchStatus {
    Idle,
    Running(crate::core::SearchProgress),
    Done(crate::core::SearchProgress),
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

    pub fn browser_mut(panel: &mut PanelState) -> Option<&mut BrowserState> {
        Some(&mut panel.browser)
    }

    pub fn browser(panel: &PanelState) -> Option<&BrowserState> {
        Some(&panel.browser)
    }

    pub fn preview_panel_side(&self) -> Option<ActivePanel> {
        if matches!(self.left_panel.mode, PanelMode::Preview(_)) {
            return Some(ActivePanel::Left);
        }
        if matches!(self.right_panel.mode, PanelMode::Preview(_)) {
            return Some(ActivePanel::Right);
        }
        None
    }

    pub fn preview_panel_mut(&mut self) -> Option<&mut PreviewState> {
        let side = self.preview_panel_side()?;
        match &mut self.panel_mut(side).mode {
            PanelMode::Preview(preview) => Some(preview),
            _ => None,
        }
    }

    pub fn edit_panel_side(&self) -> Option<ActivePanel> {
        if matches!(self.left_panel.mode, PanelMode::Edit(_)) {
            return Some(ActivePanel::Left);
        }
        if matches!(self.right_panel.mode, PanelMode::Edit(_)) {
            return Some(ActivePanel::Right);
        }
        None
    }

    pub fn edit_panel_mut(&mut self) -> Option<&mut EditState> {
        let side = self.edit_panel_side()?;
        match &mut self.panel_mut(side).mode {
            PanelMode::Edit(edit) => Some(edit),
            _ => None,
        }
    }

    pub fn select_entry(&mut self, index: usize, window_rows: usize) {
        let panel = self.get_active_panel_mut();
        let Some(browser) = Self::browser_mut(panel) else {
            return;
        };
        if index < browser.entries.len() {
            browser.selected_index = index;
            if browser.selected_index < browser.top_index {
                browser.top_index = browser.selected_index;
            } else if browser.selected_index >= browser.top_index + window_rows {
                browser.top_index = browser.selected_index + 1 - window_rows;
            }
            if self.preview_panel_side().is_some() {
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
            let Some(browser) = Self::browser(panel) else {
                return;
            };
            if browser.entries.is_empty() {
                return;
            }
            let selected_name = browser.entries[browser.selected_index].name.clone();
            match &browser.browser_mode {
                BrowserMode::Fs => (
                    Some(browser.current_path.clone()),
                    None,
                    Some(selected_name),
                ),
                BrowserMode::Container {
                    archive_path,
                    cwd,
                    kind,
                } => (
                    None,
                    Some((archive_path.clone(), cwd.clone(), *kind)),
                    Some(selected_name),
                ),
                BrowserMode::Search { .. } => (None, None, None),
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
        let Some(browser) = Self::browser_mut(panel) else {
            return;
        };
        if let Some(idx) = browser.entries.iter().position(|e| e.name == name) {
            browser.selected_index = idx;
        }
    }

    pub fn push_history(&mut self, which: ActivePanel) {
        let snapshot = {
            let panel = self.panel(which.clone());
            let Some(browser) = Self::browser(panel) else {
                return;
            };
            let selected = browser.entries.get(browser.selected_index).map(|e| {
                if matches!(browser.browser_mode, BrowserMode::Search { .. }) {
                    if let EntryLocation::Fs(path) = &e.location {
                        return format!("fs:{}", path.to_string_lossy());
                    }
                }
                e.name.clone()
            });
            PanelSnapshot {
                mode: browser.browser_mode.clone(),
                current_path: browser.current_path.clone(),
                selected_name: selected,
            }
        };
        let panel = self.panel_mut(which);
        let Some(browser) = Self::browser_mut(panel) else {
            return;
        };
        if let Some(last) = browser.history_back.last() {
            if history_key(last) == history_key(&snapshot) {
                return;
            }
        }
        browser.history_back.push(snapshot);
        browser.history_forward.clear();
    }

    pub fn pop_history_back(&mut self, which: ActivePanel) -> Option<PanelSnapshot> {
        let current = {
            let panel = self.panel(which.clone());
            let Some(browser) = Self::browser(panel) else {
                return None;
            };
            let selected = browser.entries.get(browser.selected_index).map(|e| {
                if matches!(browser.browser_mode, BrowserMode::Search { .. }) {
                    if let EntryLocation::Fs(path) = &e.location {
                        return format!("fs:{}", path.to_string_lossy());
                    }
                }
                e.name.clone()
            });
            PanelSnapshot {
                mode: browser.browser_mode.clone(),
                current_path: browser.current_path.clone(),
                selected_name: selected,
            }
        };
        let panel = self.panel_mut(which);
        let Some(browser) = Self::browser_mut(panel) else {
            return None;
        };
        let prev = browser.history_back.pop();
        if prev.is_some() {
            browser.history_forward.push(current);
        }
        prev
    }

    pub fn pop_history_forward(&mut self, which: ActivePanel) -> Option<PanelSnapshot> {
        let current = {
            let panel = self.panel(which.clone());
            let Some(browser) = Self::browser(panel) else {
                return None;
            };
            let selected = browser.entries.get(browser.selected_index).map(|e| {
                if matches!(browser.browser_mode, BrowserMode::Search { .. }) {
                    if let EntryLocation::Fs(path) = &e.location {
                        return format!("fs:{}", path.to_string_lossy());
                    }
                }
                e.name.clone()
            });
            PanelSnapshot {
                mode: browser.browser_mode.clone(),
                current_path: browser.current_path.clone(),
                selected_name: selected,
            }
        };
        let panel = self.panel_mut(which);
        let Some(browser) = Self::browser_mut(panel) else {
            return None;
        };
        let next = browser.history_forward.pop();
        if next.is_some() {
            browser.history_back.push(current);
        }
        next
    }

    pub fn prepare_rename_selected(&mut self) {
        let (path, name) = {
            let panel = self.get_active_panel();
            let Some(browser) = Self::browser(panel) else {
                return;
            };
            if browser.entries.is_empty() {
                return;
            }
            let entry = &browser.entries[browser.selected_index];
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

    pub fn prepare_edit_selected(&mut self) {
        let (path, ext) = {
            let panel = self.get_active_panel();
            let Some(browser) = Self::browser(panel) else {
                return;
            };
            if browser.entries.is_empty() {
                return;
            }
            let entry = &browser.entries[browser.selected_index];
            if entry.is_dir || entry.name == ".." {
                return;
            }
            match &entry.location {
                EntryLocation::Fs(path) => {
                    let ext = path
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string());
                    (path.clone(), ext)
                }
                _ => return,
            }
        };
        let target_panel = if let Some(side) = self.preview_panel_side() {
            side
        } else {
            match self.active_panel {
                ActivePanel::Left => ActivePanel::Right,
                ActivePanel::Right => ActivePanel::Left,
            }
        };
        let return_focus = self.active_panel.clone();
        let target_panel_clone = target_panel.clone();
        let request_id = self.edit_request_id.wrapping_add(1);
        self.edit_request_id = request_id;
        let path_to_send = {
            let panel = self.panel_mut(target_panel);
            let edit = EditState {
                path: Some(path),
                text: String::new(),
                ext,
                loading: true,
                dirty: false,
                confirm_discard: false,
                return_focus,
                highlight_key: None,
                highlight_hash: 0,
                highlight_wrap_width: 0.0,
                highlight_dirty_at: None,
                request_id,
            };
            panel.mode = PanelMode::Edit(edit);
            match &panel.mode {
                PanelMode::Edit(edit) => edit.path.clone(),
                _ => None,
            }
        };
        if let Some(path) = path_to_send {
            if self
                .edit_tx
                .send(EditLoadRequest {
                    id: request_id,
                    path,
                })
                .is_err()
            {
                if let Some(edit) = self.edit_panel_mut() {
                    edit.loading = false;
                    edit.text = "Failed to load file.".to_string();
                }
            }
        } else if let Some(edit) = self.edit_panel_mut() {
            edit.loading = false;
        }
        self.active_panel = target_panel_clone;
    }

    pub fn update_preview_for_current_selection(&mut self) {
        let (is_dir, location, key, ext) = {
            let panel = self.get_active_panel();
            let Some(browser) = Self::browser(panel) else {
                self.clear_preview();
                return;
            };
            if browser.entries.is_empty() {
                self.clear_preview();
                return;
            }
            let entry = &browser.entries[browser.selected_index];
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
        let target_panel = match self.active_panel {
            ActivePanel::Left => ActivePanel::Right,
            ActivePanel::Right => ActivePanel::Left,
        };
        let mut request_id = self.preview_request_id.wrapping_add(1);
        self.preview_request_id = request_id;
        let mut list_request: Option<(ContainerKind, path::PathBuf, u64)> = None;
        if let EntryLocation::Fs(path) = &location {
            if let Some(kind) = container_kind_from_path(path) {
                let list_id = self.preview_request_id.wrapping_add(1);
                self.preview_request_id = list_id;
                list_request = Some((kind, path.clone(), list_id));
            }
        }
        let _target_panel_clone = target_panel.clone();
        {
            let panel = self.panel_mut(target_panel);
            let preview = PreviewState {
                content: None,
                key: Some(key),
                ext,
                scroll: 0.0,
                line_height: 16.0,
                page_height: 240.0,
                can_scroll: false,
                find_open: false,
                find_query: String::new(),
                find_index: 0,
                find_focus: false,
                request_id,
            };
            panel.mode = PanelMode::Preview(preview);
        }
        let Some(preview) = self.preview_panel_mut() else {
            return;
        };
        const MAX_BYTES_TEXT: usize = 64 * 1024;
        const MAX_BYTES_BINARY: usize = 4 * 1024;
        if is_dir {
            preview.content = Some(PreviewContent::Text(format_preview_info(
                "Directory",
                &location,
            )));
            return;
        }
        match location {
            EntryLocation::Fs(path) => {
                if is_image_path(&path) {
                    preview.content = Some(PreviewContent::Image(ImageLocation::Fs(
                        std::sync::Arc::from(path),
                    )));
                    return;
                }
                if let Some((kind, archive_path, list_id)) = list_request {
                    request_id = list_id;
                    preview.request_id = request_id;
                    preview.content = Some(PreviewContent::Text(format_preview_info(
                        "Archive",
                        &EntryLocation::Container {
                            kind,
                            archive_path: archive_path.clone(),
                            inner_path: String::new(),
                        },
                    )));
                    let _ = self.preview_tx.send(PreviewRequest::ListContainer {
                        id: request_id,
                        kind,
                        archive_path,
                        max_entries: 200,
                    });
                    return;
                }
                if preview.content.is_none() {
                    preview.content = Some(PreviewContent::Text(format_preview_info(
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
                    preview.content = Some(PreviewContent::Image(ImageLocation::Container {
                        kind,
                        archive_path: archive_path.clone(),
                        inner_path: inner_path.clone(),
                    }));
                    return;
                }
                if preview.content.is_none() {
                    preview.content = Some(PreviewContent::Text(format_preview_info(
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
        if self.preview_panel_side().is_some() {
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
            let browser = Self::browser(p)?;
            if browser.entries.is_empty() {
                return None;
            }
            let entry = &browser.entries[browser.selected_index];
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
            let browser = Self::browser(other_panel)?;
            match &browser.browser_mode {
                BrowserMode::Fs => browser.current_path.clone(),
                BrowserMode::Container { .. } => {
                    return None;
                }
                BrowserMode::Search { .. } => {
                    return None;
                }
            }
        };

        Some(PendingOp::Copy { src, dst_dir, kind })
    }

    fn build_move_op(&self) -> Option<PendingOp> {
        let src = {
            let p = self.get_active_panel();
            let browser = Self::browser(p)?;
            if browser.entries.is_empty() {
                return None;
            }
            match &browser.entries[browser.selected_index].location {
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
            let browser = Self::browser(other_panel)?;
            match &browser.browser_mode {
                BrowserMode::Fs => browser.current_path.clone(),
                BrowserMode::Container { .. } => {
                    return None;
                }
                BrowserMode::Search { .. } => {
                    return None;
                }
            }
        };

        Some(PendingOp::Move { src, dst_dir })
    }

    fn build_delete_op(&self) -> Option<PendingOp> {
        let target = {
            let p = self.get_active_panel();
            let browser = Self::browser(p)?;
            if browser.entries.is_empty() {
                return None;
            }
            let entry = &browser.entries[browser.selected_index];
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
        let request_id = self.preview_request_id.wrapping_add(1);
        self.preview_request_id = request_id;
        let side = self.preview_panel_side();
        let Some(side) = side else {
            return;
        };
        let panel = self.panel_mut(side);
        panel.mode = PanelMode::Browser;
    }
}
