use std::{
    collections::{HashMap, HashSet},
    path,
    sync::{Arc, Mutex, mpsc},
    time::Instant,
};

use crate::core::{
    ActivePanel, BrowserMode, ContainerKind, DirBatch, DirEntry, EditLoadRequest, EditLoadResult,
    EntryLocation, IOResult, IOTask, ImageLocation, PreviewContent, PreviewRequest, SearchCase,
    SearchMode, SearchResult, SortMode, container_display_path, container_kind_from_path,
    format_preview_info, is_image_name, is_image_path, is_text_name, is_text_path,
};
use crate::theme::Theme;

pub struct PanelState {
    pub tabs: Vec<BrowserState>,
    pub active_tab: usize,
    pub mode: PanelMode,
}

impl PanelState {
    pub fn browser(&self) -> &BrowserState {
        &self.tabs[self.active_tab]
    }

    pub fn browser_mut(&mut self) -> &mut BrowserState {
        &mut self.tabs[self.active_tab]
    }

    pub fn new_tab(&mut self) {
        let current = &self.tabs[self.active_tab];
        let new_browser = BrowserState {
            browser_mode: current.browser_mode.clone(),
            current_path: current.current_path.clone(),
            selected_index: 0,
            entries: Vec::new(),
            entries_rx: None,
            prefer_select_name: None,
            top_index: 0,
            loading: false,
            loading_progress: None,
            container_root: current.container_root.clone(),
            dir_token: 0,
            history_back: Vec::new(),
            history_forward: Vec::new(),
            inline_rename: None,
            sort_mode: current.sort_mode,
            sort_desc: current.sort_desc,
            watching_archive: None,
            index_last_seen: 0,
            marked: std::collections::HashSet::new(),
        };
        let new_idx = self.active_tab + 1;
        self.tabs.insert(new_idx, new_browser);
        self.active_tab = new_idx;
    }

    pub fn close_tab(&mut self) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(self.active_tab);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
    }

    pub fn next_tab(&mut self) {
        if self.tabs.len() > 1 {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
        }
    }

    pub fn prev_tab(&mut self) {
        if self.tabs.len() > 1 {
            self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
        }
    }
}

pub struct FileProps {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub file_type: String,
    pub is_dir: bool,
    pub user_label: String,
    pub group_label: String,
}

pub struct FilePropsEdit {
    pub mode: u32,
    pub user: String,
    pub group: String,
}

pub struct PropsDialog {
    pub target: path::PathBuf,
    pub original: FileProps,
    pub current: FilePropsEdit,
    pub error: Option<String>,
}

pub enum PanelMode {
    Browser,
    Preview(PreviewState),
    Edit(EditState),
    Help(HelpState),
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
    pub container_root: Option<String>,
    pub dir_token: u64,
    pub history_back: Vec<PanelSnapshot>,
    pub history_forward: Vec<PanelSnapshot>,
    pub inline_rename: Option<InlineRename>,
    pub sort_mode: SortMode,
    pub sort_desc: bool,
    pub watching_archive: Option<path::PathBuf>,
    pub index_last_seen: usize,
    pub marked: std::collections::HashSet<String>,
}

pub enum InlineEditKind {
    Rename,
    NewFile,
    NewDir,
}

pub struct InlineRename {
    pub index: usize,
    pub text: String,
    pub kind: InlineEditKind,
    pub focus: bool,
}

pub struct ArchiveFullIndex {
    pub entries: Vec<(String, bool, Option<u64>)>,
    pub root: Option<String>,
    pub complete: bool,
}

pub struct ContainerDirCache {
    pub entries: Vec<DirEntry>,
    pub loading: bool,
    pub loading_progress: Option<(usize, Option<usize>)>,
    pub entries_rx: Option<mpsc::Receiver<DirBatch>>,
    pub selected_index: usize,
    pub top_index: usize,
    pub root: Option<String>,
}

pub struct PreviewState {
    pub content: Option<PreviewContent>,
    pub key: Option<String>,
    pub ext: Option<String>,
    pub scroll: f32,
    pub line_height: f32,
    pub page_height: f32,
    pub max_scroll: f32,
    pub can_scroll: bool,
    pub find_open: bool,
    pub find_query: String,
    pub find_index: usize,
    pub find_focus: bool,
    pub request_id: u64,
    pub wrap: bool,
    pub show_whitespace: bool,
    pub bytes_per_row: usize,
    pub bytes_per_row_auto: bool,
    pub loading_since: Option<Instant>,
    /// Image zoom: 0.0 = fit-to-panel, >0 = percentage (1.0 = 100%).
    pub image_zoom: f32,
    /// Image pan offset (x, y) in pixels, used when zoomed image exceeds panel.
    pub image_pan: [f32; 2],
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

pub struct HelpState {
    pub return_focus: ActivePanel,
}
#[derive(Clone)]
pub struct PanelSnapshot {
    pub mode: BrowserMode,
    pub current_path: path::PathBuf,
    pub selected_name: Option<String>,
}

fn history_key(snapshot: &PanelSnapshot) -> String {
    match snapshot.mode {
        BrowserMode::Fs => format!("fs:{}", snapshot.current_path.to_string_lossy()),
        BrowserMode::Container {
            kind,
            ref archive_path,
            ref cwd,
            ref root,
        } => format!(
            "container:{}:{}:{}:{}",
            match kind {
                ContainerKind::Zip => "zip",
                ContainerKind::Tar => "tar",
                ContainerKind::TarGz => "tar.gz",
                ContainerKind::TarBz2 => "tar.bz2",
            },
            archive_path.to_string_lossy(),
            cwd,
            root.as_deref().unwrap_or_default()
        ),
        BrowserMode::Search {
            ref root,
            ref query,
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
    pub preview_return_focus: Option<ActivePanel>,
    pub allow_external_open: bool,
    pub wake: Option<Arc<dyn Fn() + Send + Sync>>,
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
    pub container_dir_cache: HashMap<(path::PathBuf, String, ContainerKind), ContainerDirCache>,
    pub archive_index: HashMap<path::PathBuf, Arc<Mutex<ArchiveFullIndex>>>,
    pub props_dialog: Option<PropsDialog>,
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
    pub refresh_tick: u64,
    pub update_status: UpdateStatus,
    pub update_rx: Option<mpsc::Receiver<UpdateStatus>>,
    pub gpu_info: String,
}

#[derive(Clone)]
pub struct CopyItem {
    pub src: EntryLocation,
    pub kind: CopyKind,
}

#[derive(Clone)]
pub enum PendingOp {
    Copy {
        items: Vec<CopyItem>,
        dst_dir: path::PathBuf,
    },
    Move {
        sources: Vec<path::PathBuf>,
        dst_dir: path::PathBuf,
    },
    Delete {
        targets: Vec<path::PathBuf>,
    },
    Rename {
        src: path::PathBuf,
    },
    Pack {
        sources: Vec<path::PathBuf>,
        dst_dir: path::PathBuf,
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

#[derive(Clone)]
pub enum UpdateStatus {
    /// Feature not compiled in, or not checking
    Disabled,
    /// Background check in progress
    Checking,
    /// Already on latest version
    UpToDate,
    /// A newer version is available
    Available(String),
    /// Download + install in progress
    Installing(String),
    /// Successfully installed, restart needed
    Installed(String),
    /// Check or install failed
    Failed(String),
}

pub struct AsyncStatus {
    pub io_in_flight: usize,
    pub io_cancel_requested: bool,
    pub dir_size_pending: usize,
    pub search: SearchStatus,
    pub update: UpdateStatus,
    pub gpu_info: String,
}

impl AppState {
    pub fn poll_update_status(&mut self) {
        if let Some(ref rx) = self.update_rx
            && let Ok(status) = rx.try_recv()
        {
            self.update_status = status;
            self.update_rx = None;
        }
    }

    pub fn async_status(&self) -> AsyncStatus {
        AsyncStatus {
            io_in_flight: self.io_in_flight,
            io_cancel_requested: self.io_cancel_requested,
            dir_size_pending: self.dir_size_pending.len(),
            search: self.search_status,
            update: self.update_status.clone(),
            gpu_info: self.gpu_info.clone(),
        }
    }

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
        self.panel(self.active_panel)
    }

    pub fn get_active_panel_mut(&mut self) -> &mut PanelState {
        self.panel_mut(self.active_panel)
    }

    pub fn preview_panel_side(&self) -> Option<ActivePanel> {
        let PanelState {
            mode: ref left_mode,
            ..
        } = self.left_panel;
        if let PanelMode::Preview(_) = *left_mode {
            return Some(ActivePanel::Left);
        }
        let PanelState {
            mode: ref right_mode,
            ..
        } = self.right_panel;
        if let PanelMode::Preview(_) = *right_mode {
            return Some(ActivePanel::Right);
        }
        None
    }

    pub fn preview_panel_mut(&mut self) -> Option<&mut PreviewState> {
        let side = self.preview_panel_side()?;
        let panel = self.panel_mut(side);
        match panel.mode {
            PanelMode::Preview(ref mut preview) => Some(preview),
            _ => None,
        }
    }

    pub fn preview_panel(&self) -> Option<&PreviewState> {
        let side = self.preview_panel_side()?;
        let panel = self.panel(side);
        match panel.mode {
            PanelMode::Preview(ref preview) => Some(preview),
            _ => None,
        }
    }

    pub fn edit_panel_side(&self) -> Option<ActivePanel> {
        let PanelState {
            mode: ref left_mode,
            ..
        } = self.left_panel;
        if let PanelMode::Edit(_) = *left_mode {
            return Some(ActivePanel::Left);
        }
        let PanelState {
            mode: ref right_mode,
            ..
        } = self.right_panel;
        if let PanelMode::Edit(_) = *right_mode {
            return Some(ActivePanel::Right);
        }
        None
    }

    pub fn help_panel_side(&self) -> Option<ActivePanel> {
        let PanelState {
            mode: ref left_mode,
            ..
        } = self.left_panel;
        if let PanelMode::Help(_) = *left_mode {
            return Some(ActivePanel::Left);
        }
        let PanelState {
            mode: ref right_mode,
            ..
        } = self.right_panel;
        if let PanelMode::Help(_) = *right_mode {
            return Some(ActivePanel::Right);
        }
        None
    }

    pub fn help_panel(&self, which: ActivePanel) -> Option<&HelpState> {
        let panel = self.panel(which);
        match panel.mode {
            PanelMode::Help(ref help) => Some(help),
            _ => None,
        }
    }

    pub fn edit_panel(&self) -> Option<&EditState> {
        let side = self.edit_panel_side()?;
        let panel = self.panel(side);
        match panel.mode {
            PanelMode::Edit(ref edit) => Some(edit),
            _ => None,
        }
    }

    pub fn edit_panel_mut(&mut self) -> Option<&mut EditState> {
        let side = self.edit_panel_side()?;
        let panel = self.panel_mut(side);
        match panel.mode {
            PanelMode::Edit(ref mut edit) => Some(edit),
            _ => None,
        }
    }

    pub fn select_entry(&mut self, index: usize, window_rows: usize) {
        let panel = self.get_active_panel_mut();
        let browser = panel.browser_mut();
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

    pub fn swap_panels(&mut self) {
        std::mem::swap(&mut self.left_panel, &mut self.right_panel);
    }

    pub fn store_current_selection_memory(&mut self) {
        self.store_selection_memory_for(self.active_panel);
    }

    pub fn store_selection_memory_for(&mut self, which: ActivePanel) {
        let (fs_key, container_key, selected_name_opt) = {
            let panel = self.panel(which);
            let browser = panel.browser();
            if browser.entries.is_empty() {
                return;
            }
            let selected_name = browser.entries[browser.selected_index].name.clone();
            match browser.browser_mode {
                BrowserMode::Fs => (
                    Some(browser.current_path.clone()),
                    None,
                    Some(selected_name),
                ),
                BrowserMode::Container {
                    ref archive_path,
                    ref cwd,
                    kind,
                    root: _,
                } => (
                    None,
                    Some((archive_path.clone(), cwd.clone(), kind)),
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

    pub fn stash_container_cache(&mut self, which: ActivePanel) {
        let (key, cache) = {
            let panel = self.panel_mut(which);
            let browser = panel.browser_mut();
            let BrowserMode::Container {
                ref archive_path,
                ref cwd,
                kind,
                root: _,
            } = browser.browser_mode
            else {
                return;
            };
            let key = (archive_path.clone(), cwd.clone(), kind);
            let cache = ContainerDirCache {
                entries: browser.entries.clone(),
                loading: browser.loading,
                loading_progress: browser.loading_progress,
                entries_rx: browser.entries_rx.take(),
                selected_index: browser.selected_index,
                top_index: browser.top_index,
                root: browser.container_root.clone(),
            };
            (key, cache)
        };
        self.container_dir_cache.insert(key, cache);
    }

    pub fn select_entry_by_name(&mut self, which: ActivePanel, name: &str) {
        let panel = self.panel_mut(which);
        let browser = panel.browser_mut();
        if let Some(idx) = browser.entries.iter().position(|e| e.name == name) {
            browser.selected_index = idx;
        }
    }

    pub fn push_history(&mut self, which: ActivePanel) {
        let snapshot = {
            let panel = self.panel(which);
            let browser = panel.browser();
            let selected = browser.entries.get(browser.selected_index).map(|e| {
                if matches!(browser.browser_mode, BrowserMode::Search { .. })
                    && let EntryLocation::Fs(path) = e.location.clone()
                {
                    return format!("fs:{}", path.to_string_lossy());
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
        let browser = panel.browser_mut();
        if let Some(last) = browser.history_back.last()
            && history_key(last) == history_key(&snapshot)
        {
            return;
        }
        browser.history_back.push(snapshot);
        browser.history_forward.clear();
    }

    pub fn pop_history_back(&mut self, which: ActivePanel) -> Option<PanelSnapshot> {
        let current = {
            let panel = self.panel(which);
            let browser = panel.browser();
            let selected = browser.entries.get(browser.selected_index).map(|e| {
                if matches!(browser.browser_mode, BrowserMode::Search { .. })
                    && let EntryLocation::Fs(path) = e.location.clone()
                {
                    return format!("fs:{}", path.to_string_lossy());
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
        let browser = panel.browser_mut();
        let prev = browser.history_back.pop();
        if prev.is_some() {
            browser.history_forward.push(current);
        }
        prev
    }

    pub fn pop_history_forward(&mut self, which: ActivePanel) -> Option<PanelSnapshot> {
        let current = {
            let panel = self.panel(which);
            let browser = panel.browser();
            let selected = browser.entries.get(browser.selected_index).map(|e| {
                if matches!(browser.browser_mode, BrowserMode::Search { .. })
                    && let EntryLocation::Fs(path) = e.location.clone()
                {
                    return format!("fs:{}", path.to_string_lossy());
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
        let browser = panel.browser_mut();
        let next = browser.history_forward.pop();
        if next.is_some() {
            browser.history_back.push(current);
        }
        next
    }

    pub fn prepare_rename_selected(&mut self) {
        let name = {
            let panel = self.get_active_panel();
            let browser = panel.browser();
            if browser.entries.is_empty() {
                return;
            }
            let entry = &browser.entries[browser.selected_index];
            if entry.name == ".." || entry.is_dir && !matches!(entry.location, EntryLocation::Fs(_))
            {
                return;
            }
            if let EntryLocation::Fs(path) = entry.location.clone() {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        };
        if let Some(name) = name {
            let panel = self.get_active_panel_mut();
            let browser = panel.browser_mut();
            browser.inline_rename = Some(InlineRename {
                index: browser.selected_index,
                text: name,
                kind: InlineEditKind::Rename,
                focus: true,
            });
            self.rename_input = None;
            self.pending_op = None;
            self.rename_focus = false;
        }
    }

    pub fn start_inline_new_file(&mut self) {
        let target_dir = {
            let panel = self.get_active_panel();
            let browser = panel.browser();
            if !matches!(browser.browser_mode, BrowserMode::Fs) {
                return;
            }
            browser.current_path.clone()
        };
        let panel = self.get_active_panel_mut();
        let browser = panel.browser_mut();
        let base = "new_file".to_string();
        let mut candidate = base.clone();
        let mut counter = 1;
        while browser.entries.iter().any(|e| e.name == candidate) {
            candidate = format!("{base}_{counter}");
            counter += 1;
        }
        let insert_at = browser
            .entries
            .iter()
            .position(|e| e.name != "..")
            .unwrap_or(browser.entries.len());
        let new_path = target_dir.join(&candidate);
        browser.entries.insert(
            insert_at,
            DirEntry {
                name: candidate.clone(),
                is_dir: false,
                is_symlink: false,
                link_target: None,
                location: EntryLocation::Fs(new_path),
                size: None,
                modified: None,
            },
        );
        browser.selected_index = insert_at;
        browser.inline_rename = Some(InlineRename {
            index: insert_at,
            text: candidate,
            kind: InlineEditKind::NewFile,
            focus: true,
        });
    }

    pub fn start_inline_new_dir(&mut self) {
        let target_dir = {
            let panel = self.get_active_panel();
            let browser = panel.browser();
            if !matches!(browser.browser_mode, BrowserMode::Fs) {
                return;
            }
            browser.current_path.clone()
        };
        let panel = self.get_active_panel_mut();
        let browser = panel.browser_mut();
        let base = "new_dir".to_string();
        let mut candidate = base.clone();
        let mut counter = 1;
        while browser.entries.iter().any(|e| e.name == candidate) {
            candidate = format!("{base}_{counter}");
            counter += 1;
        }
        // Insert among directories, after ".." but before files
        let insert_at = browser
            .entries
            .iter()
            .position(|e| !e.is_dir)
            .unwrap_or(browser.entries.len());
        let new_path = target_dir.join(&candidate);
        browser.entries.insert(
            insert_at,
            DirEntry {
                name: candidate.clone(),
                is_dir: true,
                is_symlink: false,
                link_target: None,
                location: EntryLocation::Fs(new_path),
                size: None,
                modified: None,
            },
        );
        browser.selected_index = insert_at;
        browser.inline_rename = Some(InlineRename {
            index: insert_at,
            text: candidate,
            kind: InlineEditKind::NewDir,
            focus: true,
        });
    }

    pub fn prepare_edit_selected(&mut self) {
        let (path, ext) = {
            let panel = self.get_active_panel();
            let browser = panel.browser();
            if browser.entries.is_empty() {
                return;
            }
            let entry = &browser.entries[browser.selected_index];
            if entry.is_dir || entry.name == ".." {
                return;
            }
            match entry.location.clone() {
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
        let return_focus = self.active_panel;
        let target_panel_clone = target_panel;
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
            match panel.mode {
                PanelMode::Edit(ref edit) => edit.path.clone(),
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
                && let Some(edit) = self.edit_panel_mut()
            {
                edit.loading = false;
                edit.text = "Failed to load file.".to_string();
            }
        } else if let Some(edit) = self.edit_panel_mut() {
            edit.loading = false;
        }
        self.active_panel = target_panel_clone;
    }

    pub fn update_preview_for_current_selection(&mut self) {
        let (is_dir, location, key, ext) = {
            let panel = self.get_active_panel();
            let browser = panel.browser();
            if browser.entries.is_empty() {
                self.clear_preview();
                return;
            }
            let entry = &browser.entries[browser.selected_index];
            let ext = std::path::Path::new(&entry.name)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_string());
            let key = match entry.location.clone() {
                EntryLocation::Fs(path) => path.to_string_lossy().into_owned(),
                EntryLocation::Container {
                    kind,
                    archive_path,
                    inner_path,
                } => container_display_path(kind, &archive_path, &inner_path),
            };
            (entry.is_dir, entry.location.clone(), key, ext)
        };
        let target_panel = match self.active_panel {
            ActivePanel::Left => ActivePanel::Right,
            ActivePanel::Right => ActivePanel::Left,
        };
        self.preview_return_focus = Some(self.active_panel);
        let mut request_id = self.preview_request_id.wrapping_add(1);
        self.preview_request_id = request_id;
        // no capture
        let mut list_request: Option<(ContainerKind, path::PathBuf, u64)> = None;
        if let EntryLocation::Fs(path) = location.clone()
            && let Some(kind) = container_kind_from_path(&path)
        {
            let list_id = self.preview_request_id.wrapping_add(1);
            self.preview_request_id = list_id;
            list_request = Some((kind, path.clone(), list_id));
        }
        let _target_panel_clone = target_panel;
        {
            let panel = self.panel_mut(target_panel);
            let preview = PreviewState {
                content: None,
                key: Some(key),
                ext,
                scroll: 0.0,
                line_height: 16.0,
                page_height: 240.0,
                max_scroll: 0.0,
                can_scroll: false,
                find_open: false,
                find_query: String::new(),
                find_index: 0,
                find_focus: false,
                request_id,
                wrap: true,
                show_whitespace: false,
                bytes_per_row: 16,
                bytes_per_row_auto: true,
                loading_since: Some(Instant::now()),
                image_zoom: 0.0,
                image_pan: [0.0, 0.0],
            };
            panel.mode = PanelMode::Preview(preview);
        }
        let Some(preview) = self.preview_panel_mut() else {
            return;
        };
        if is_dir {
            preview.content = Some(PreviewContent::Text(format_preview_info(
                "Directory",
                &location,
            )));
            preview.loading_since = None;
            return;
        }
        match location {
            EntryLocation::Fs(path) => {
                if is_image_path(&path) {
                    preview.content = Some(PreviewContent::Image(ImageLocation::Fs(
                        std::sync::Arc::from(path),
                    )));
                    preview.loading_since = None;
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
                    preview.loading_since = None;
                    let _ = self.preview_tx.send(PreviewRequest::ListContainer {
                        id: request_id,
                        kind,
                        archive_path,
                        max_entries: 200,
                    });
                    return;
                }
                let max_bytes = if is_text_path(&path) {
                    Some(64 * 1024)
                } else {
                    Some(8 * 1024)
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
                    preview.loading_since = None;
                    return;
                }
                let max_bytes = if is_text_name(&inner_path) {
                    Some(64 * 1024)
                } else {
                    Some(8 * 1024)
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

    pub fn toggle_help(&mut self) {
        if let Some(side) = self.help_panel_side() {
            let fallback = self.active_panel;
            let panel = self.panel_mut(side);
            let return_focus = match panel.mode {
                PanelMode::Help(HelpState { return_focus }) => return_focus,
                _ => fallback,
            };
            panel.mode = PanelMode::Browser;
            self.active_panel = return_focus;
            return;
        }
        let target_panel = match self.active_panel {
            ActivePanel::Left => ActivePanel::Right,
            ActivePanel::Right => ActivePanel::Left,
        };
        let return_focus = self.active_panel;
        let panel = self.panel_mut(target_panel);
        panel.mode = PanelMode::Help(HelpState { return_focus });
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

    pub fn prepare_pack_selected(&mut self) {
        if self.pending_op.is_some() {
            return;
        }
        if let Some(op) = self.build_pack_op() {
            self.pending_op = Some(op);
            // Pre-fill archive name based on first source
            let name = match self.pending_op {
                Some(PendingOp::Pack { ref sources, .. }) => sources
                    .first()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(|n| format!("{n}.zip"))
                    .unwrap_or_else(|| "archive.zip".to_string()),
                _ => "archive.zip".to_string(),
            };
            self.rename_input = Some(name);
            self.rename_focus = true;
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

    fn enqueue_io(&mut self, task: IOTask) {
        if let Err(e) = self.io_tx.send(task) {
            eprintln!("Failed to enqueue IO: {e}");
        } else {
            self.io_in_flight = self.io_in_flight.saturating_add(1);
        }
    }

    pub fn enqueue_pending_op(&mut self, op: &PendingOp) {
        match *op {
            PendingOp::Copy {
                ref items,
                ref dst_dir,
            } => {
                for item in items {
                    let task = match item.src {
                        EntryLocation::Fs(ref path) => IOTask::Copy {
                            src: path.clone(),
                            dst_dir: dst_dir.clone(),
                        },
                        EntryLocation::Container {
                            kind: container_kind,
                            ref archive_path,
                            ref inner_path,
                        } => match item.kind {
                            CopyKind::File => IOTask::CopyContainer {
                                kind: container_kind,
                                archive_path: archive_path.clone(),
                                inner_path: inner_path.clone(),
                                dst_dir: dst_dir.clone(),
                                display_name: item.src.display_name(),
                            },
                            CopyKind::Directory => IOTask::CopyContainerDir {
                                kind: container_kind,
                                archive_path: archive_path.clone(),
                                inner_path: inner_path.clone(),
                                dst_dir: dst_dir.clone(),
                                display_name: item.src.display_name(),
                            },
                        },
                    };
                    self.enqueue_io(task);
                }
            }
            PendingOp::Move {
                ref sources,
                ref dst_dir,
            } => {
                for src in sources {
                    self.enqueue_io(IOTask::Move {
                        src: src.clone(),
                        dst_dir: dst_dir.clone(),
                    });
                }
            }
            PendingOp::Delete { ref targets } => {
                for target in targets {
                    self.enqueue_io(IOTask::Delete {
                        target: target.clone(),
                    });
                }
            }
            PendingOp::Rename { ref src } => {
                if let Some(new_name) = self.rename_input.clone() {
                    self.enqueue_io(IOTask::Rename {
                        src: src.clone(),
                        new_name,
                    });
                }
            }
            PendingOp::Pack {
                ref sources,
                ref dst_dir,
            } => {
                if let Some(archive_name) = self.rename_input.clone() {
                    let archive_path = dst_dir.join(&archive_name);
                    let kind = crate::core::container_kind_from_path(&archive_path)
                        .unwrap_or(ContainerKind::Zip);
                    self.enqueue_io(IOTask::Pack {
                        sources: sources.clone(),
                        archive_path,
                        kind,
                    });
                }
            }
        }
        // Clear marks after operation is enqueued
        self.get_active_panel_mut().browser_mut().marked.clear();
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

    /// Returns indices of marked entries, or just the cursor entry if nothing is marked.
    /// Excludes ".." entries.
    fn effective_selection(&self) -> Vec<usize> {
        let browser = self.get_active_panel().browser();
        if browser.entries.is_empty() {
            return Vec::new();
        }
        if !browser.marked.is_empty() {
            let mut indices: Vec<usize> = (0..browser.entries.len())
                .filter(|i| browser.marked.contains(&browser.entries[*i].name))
                .collect();
            indices.sort();
            return indices;
        }
        let idx = browser.selected_index;
        if idx < browser.entries.len() && browser.entries[idx].name != ".." {
            vec![idx]
        } else {
            Vec::new()
        }
    }

    fn other_panel_fs_dir(&self) -> Option<path::PathBuf> {
        let other = match self.active_panel {
            ActivePanel::Left => &self.right_panel,
            ActivePanel::Right => &self.left_panel,
        };
        match other.browser().browser_mode {
            BrowserMode::Fs => Some(other.browser().current_path.clone()),
            _ => None,
        }
    }

    fn build_copy_op(&self) -> Option<PendingOp> {
        let indices = self.effective_selection();
        if indices.is_empty() {
            return None;
        }
        let dst_dir = self.other_panel_fs_dir()?;
        let browser = self.get_active_panel().browser();
        let items: Vec<CopyItem> = indices
            .iter()
            .map(|&i| {
                let entry = &browser.entries[i];
                CopyItem {
                    src: entry.location.clone(),
                    kind: if entry.is_dir {
                        CopyKind::Directory
                    } else {
                        CopyKind::File
                    },
                }
            })
            .collect();
        Some(PendingOp::Copy { items, dst_dir })
    }

    fn build_move_op(&self) -> Option<PendingOp> {
        let indices = self.effective_selection();
        if indices.is_empty() {
            return None;
        }
        let dst_dir = self.other_panel_fs_dir()?;
        let browser = self.get_active_panel().browser();
        let sources: Vec<path::PathBuf> = indices
            .iter()
            .filter_map(|&i| match browser.entries[i].location {
                EntryLocation::Fs(ref path) => Some(path.clone()),
                EntryLocation::Container { .. } => None,
            })
            .collect();
        if sources.is_empty() {
            return None;
        }
        Some(PendingOp::Move { sources, dst_dir })
    }

    fn build_delete_op(&self) -> Option<PendingOp> {
        let indices = self.effective_selection();
        if indices.is_empty() {
            return None;
        }
        let browser = self.get_active_panel().browser();
        let targets: Vec<path::PathBuf> = indices
            .iter()
            .filter_map(|&i| match browser.entries[i].location {
                EntryLocation::Fs(ref path) => Some(path.clone()),
                EntryLocation::Container { .. } => None,
            })
            .collect();
        if targets.is_empty() {
            return None;
        }
        Some(PendingOp::Delete { targets })
    }

    fn build_pack_op(&self) -> Option<PendingOp> {
        let indices = self.effective_selection();
        if indices.is_empty() {
            return None;
        }
        let browser = self.get_active_panel().browser();
        // Only pack filesystem entries
        let sources: Vec<path::PathBuf> = indices
            .iter()
            .filter_map(|&i| match browser.entries[i].location {
                EntryLocation::Fs(ref path) => Some(path.clone()),
                EntryLocation::Container { .. } => None,
            })
            .collect();
        if sources.is_empty() {
            return None;
        }
        // Archive goes into the current panel's directory
        let dst_dir = browser.current_path.clone();
        Some(PendingOp::Pack { sources, dst_dir })
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
            self.theme
                .external
                .iter()
                .map(|pair| pair.0.clone())
                .collect()
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
        if let Some(return_focus) = self.preview_return_focus.take() {
            self.active_panel = return_focus;
        }
    }
}
