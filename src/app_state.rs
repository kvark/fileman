use std::{collections::HashMap, path, sync::mpsc};

use crate::core::{
    ActivePanel, DirBatch, DirEntry, EntryLocation, IOTask, PanelMode, PreviewContent,
    PreviewRequest, format_preview_info, is_image_path,
};
use crate::theme::Theme;

pub struct PanelState {
    pub current_path: path::PathBuf, // For Fs mode: real fs path. For Zip: archive file path.
    pub mode: PanelMode,
    pub selected_index: usize,
    pub entries: Vec<DirEntry>,
    pub entries_rx: Option<mpsc::Receiver<DirBatch>>,
    pub prefer_select_name: Option<String>,
    pub top_index: usize,
}

pub struct AppState {
    pub left_panel: PanelState,
    pub right_panel: PanelState,
    pub active_panel: ActivePanel,
    pub preview: Option<PreviewContent>,
    pub preview_tx: mpsc::Sender<PreviewRequest>,
    pub preview_rx: mpsc::Receiver<(u64, PreviewContent)>,
    pub preview_request_id: u64,
    pub io_tx: mpsc::Sender<IOTask>,
    pub fs_last_selected_name: HashMap<path::PathBuf, String>,
    pub zip_last_selected_name: HashMap<(path::PathBuf, String), String>,
    pub theme: Theme,
    pub theme_picker_open: bool,
    pub theme_picker_selected: Option<usize>,
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
        let (fs_key, zip_key, selected_name_opt) = {
            let panel = self.get_active_panel();
            if panel.entries.is_empty() {
                return;
            }
            let selected_name = panel.entries[panel.selected_index].name.clone();
            match &panel.mode {
                PanelMode::Fs => (Some(panel.current_path.clone()), None, Some(selected_name)),
                PanelMode::Zip {
                    archive_path, cwd, ..
                } => (
                    None,
                    Some((archive_path.clone(), cwd.clone())),
                    Some(selected_name),
                ),
            }
        };
        if let Some(selected_name) = selected_name_opt {
            if let Some(path) = fs_key {
                self.fs_last_selected_name.insert(path, selected_name);
            } else if let Some((ap, cwd)) = zip_key {
                self.zip_last_selected_name.insert((ap, cwd), selected_name);
            }
        }
    }

    pub fn select_entry_by_name(&mut self, which: ActivePanel, name: &str) {
        let panel = self.panel_mut(which);
        if let Some(idx) = panel.entries.iter().position(|e| e.name == name) {
            panel.selected_index = idx;
        }
    }

    pub fn update_preview_for_current_selection(&mut self) {
        let (is_dir, location) = {
            let panel = self.get_active_panel();
            if panel.entries.is_empty() {
                self.clear_preview();
                return;
            }
            let entry = &panel.entries[panel.selected_index];
            (entry.is_dir, entry.location.clone())
        };
        self.preview_request_id = self.preview_request_id.wrapping_add(1);
        let request_id = self.preview_request_id;
        const MAX_BYTES: usize = 64 * 1024;
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
                    self.preview = Some(PreviewContent::Image(std::sync::Arc::from(path)));
                    return;
                }
                if self.preview.is_none() {
                    self.preview = Some(PreviewContent::Text(format_preview_info(
                        "File",
                        &EntryLocation::Fs(path.clone()),
                    )));
                }
                let _ = self.preview_tx.send(PreviewRequest::Read {
                    id: request_id,
                    location: EntryLocation::Fs(path),
                    max_bytes: MAX_BYTES,
                });
            }
            EntryLocation::Zip {
                archive_path,
                inner_path,
            } => {
                if self.preview.is_none() {
                    self.preview = Some(PreviewContent::Text(format_preview_info(
                        "File",
                        &EntryLocation::Zip {
                            archive_path: archive_path.clone(),
                            inner_path: inner_path.clone(),
                        },
                    )));
                }
                let _ = self.preview_tx.send(PreviewRequest::Read {
                    id: request_id,
                    location: EntryLocation::Zip {
                        archive_path,
                        inner_path,
                    },
                    max_bytes: MAX_BYTES,
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

    pub fn enqueue_copy_selected(&mut self) {
        let src = {
            let p = self.get_active_panel();
            if p.entries.is_empty() {
                return;
            }
            match &p.entries[p.selected_index].location {
                EntryLocation::Fs(path) => path.clone(),
                EntryLocation::Zip { .. } => {
                    return;
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
                PanelMode::Zip { .. } => {
                    return;
                }
            }
        };

        if let Err(e) = self.io_tx.send(IOTask::Copy {
            src: src.clone(),
            dst_dir: dst_dir.clone(),
        }) {
            eprintln!("Failed to enqueue copy: {e}");
        } else {
            log::info!(
                "Enqueued copy: {} -> {}",
                src.to_string_lossy(),
                dst_dir.to_string_lossy()
            );
        }
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
        self.preview_request_id = self.preview_request_id.wrapping_add(1);
    }
}
