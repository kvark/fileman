use gpui::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read},
    path::{self, Path},
    sync::{Arc, mpsc},
    thread,
};

#[derive(Clone)]
enum EntryLocation {
    Fs(path::PathBuf),
    Zip {
        archive_path: path::PathBuf,
        inner_path: String, // no leading slash, '' means root
    },
}

struct DirEntry {
    name: String,
    is_dir: bool,
    location: EntryLocation,
}

#[derive(Clone, PartialEq)]
enum ActivePanel {
    Left,
    Right,
}

enum PanelMode {
    Fs,
    Zip {
        archive_path: path::PathBuf,
        cwd: String,
    },
}

struct PanelState {
    current_path: path::PathBuf, // For Fs mode: real fs path. For Zip: archive file path.
    mode: PanelMode,
    selected_index: usize,
    entries: Vec<DirEntry>,
}

enum PreviewContent {
    Text(String),
    Image(Arc<Path>),
}

enum IOTask {
    Copy {
        src: path::PathBuf,
        dst_dir: path::PathBuf,
    },
}

// Models
struct FileSystemModel {
    left_panel: PanelState,
    right_panel: PanelState,
    active_panel: ActivePanel,
    preview: Option<PreviewContent>,
    io_tx: mpsc::Sender<IOTask>,
}

fn start_io_worker() -> mpsc::Sender<IOTask> {
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

fn copy_recursively(src: &Path, dst_dir: &Path) -> io::Result<()> {
    if src.is_dir() {
        let dest = dst_dir.join(src.file_name().unwrap());
        fs::create_dir_all(&dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                copy_recursively(&path, &dest)?;
            } else {
                fs::copy(&path, dest.join(entry.file_name()))?;
            }
        }
    } else {
        let dest = dst_dir.join(src.file_name().unwrap());
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dest)?;
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let cur_dir = std::env::current_dir()?;
    let io_tx = start_io_worker();

    gpui::Application::new().run(move |cx| {
        cx.open_window(
            gpui::WindowOptions {
                focus: true,
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Dual Panel File Manager".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, app| {
                let io_tx_clone = io_tx.clone();
                let fs_entity = app.new(move |_| FileSystemModel {
                    left_panel: PanelState {
                        current_path: cur_dir.clone(),
                        mode: PanelMode::Fs,
                        selected_index: 0,
                        entries: Vec::new(),
                    },
                    right_panel: PanelState {
                        current_path: cur_dir.clone(),
                        mode: PanelMode::Fs,
                        selected_index: 0,
                        entries: Vec::new(),
                    },
                    active_panel: ActivePanel::Left,
                    preview: None,
                    io_tx: io_tx_clone.clone(),
                });

                // Load initial directories
                app.update_entity(&fs_entity, |model: &mut FileSystemModel, cx| {
                    model.load_fs_directory(
                        model.left_panel.current_path.clone(),
                        ActivePanel::Left,
                        cx,
                    );
                    model.load_fs_directory(
                        model.right_panel.current_path.clone(),
                        ActivePanel::Right,
                        cx,
                    );
                });

                let view = app.new(|cx| FileManagerView {
                    model: fs_entity,
                    focus_handle: {
                        window.focus(&cx.focus_handle());
                        cx.focus_handle().clone()
                    },
                });

                window.activate_window();
                app.activate(true);
                view
            },
        )
        .unwrap();
    });
    Ok(())
}

impl FileSystemModel {
    #[profiling::function]
    fn load_fs_directory(
        &mut self,
        path: path::PathBuf,
        target_panel: ActivePanel,
        _cx: &mut gpui::Context<Self>,
    ) {
        let entries_result = Self::read_fs_directory(&path);
        let panel_state = self.panel_mut(target_panel);

        match entries_result {
            Ok(entries) => {
                panel_state.current_path = path.clone();
                panel_state.mode = PanelMode::Fs;
                panel_state.entries = entries;
                panel_state.selected_index = 0;
            }
            Err(e) => {
                eprintln!("Error loading directory: {}", e);
            }
        }
    }

    #[profiling::function]
    fn load_zip_directory(
        &mut self,
        archive_path: path::PathBuf,
        cwd: String,
        target_panel: ActivePanel,
        _cx: &mut gpui::Context<Self>,
    ) {
        let entries_result = Self::read_zip_directory(&archive_path, &cwd);
        let panel_state = self.panel_mut(target_panel);

        match entries_result {
            Ok(entries) => {
                panel_state.current_path = archive_path.clone();
                panel_state.mode = PanelMode::Zip { archive_path, cwd };
                panel_state.entries = entries;
                panel_state.selected_index = 0;
            }
            Err(e) => {
                eprintln!("Error loading zip: {}", e);
            }
        }
    }

    #[profiling::function]
    fn read_fs_directory(path: &path::Path) -> anyhow::Result<Vec<DirEntry>> {
        let mut entries = Vec::new();

        let mut read_dir = fs::read_dir(path)?;
        let mut dir_entries = Vec::new();

        while let Some(entry) = read_dir.next() {
            let entry = entry?;
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy().to_string();

            let file_type = entry.file_type()?;
            let is_dir = file_type.is_dir();

            dir_entries.push(DirEntry {
                name: file_name,
                is_dir,
                location: EntryLocation::Fs(entry.path()),
            });
        }

        dir_entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

        if path.parent().is_some() {
            entries.push(DirEntry {
                name: "..".to_string(),
                is_dir: true,
                location: EntryLocation::Fs(path.parent().unwrap().to_path_buf()),
            });
        }

        entries.extend(dir_entries);

        Ok(entries)
    }

    #[profiling::function]
    fn read_zip_directory(archive_path: &Path, cwd: &str) -> anyhow::Result<Vec<DirEntry>> {
        let file = fs::File::open(archive_path)?;
        let mut zip = zip::ZipArchive::new(file)?;
        let mut dirs: HashSet<String> = HashSet::new();
        let mut files: Vec<String> = Vec::new();

        let prefix = if cwd.is_empty() {
            "".to_string()
        } else {
            format!("{}/", cwd.trim_end_matches('/'))
        };

        for i in 0..zip.len() {
            let entry = zip.by_index(i)?;
            let name = entry.name();
            if !name.starts_with(&prefix) {
                continue;
            }
            let rem = &name[prefix.len()..];
            if rem.is_empty() {
                continue;
            }
            if let Some(slash) = rem.find('/') {
                let dir = rem[..slash].to_string();
                dirs.insert(dir);
            } else {
                files.push(rem.to_string());
            }
        }

        let mut entries: Vec<DirEntry> = Vec::new();

        // Parent entry
        if !cwd.is_empty() {
            let parent = cwd
                .trim_end_matches('/')
                .rsplit_once('/')
                .map(|(p, _)| p.to_string())
                .unwrap_or_else(|| "".to_string());
            entries.push(DirEntry {
                name: "..".into(),
                is_dir: true,
                location: EntryLocation::Zip {
                    archive_path: archive_path.to_path_buf(),
                    inner_path: parent,
                },
            });
        } else {
            // leaving the archive to its parent FS directory
            if let Some(parent) = archive_path.parent() {
                entries.push(DirEntry {
                    name: "..".into(),
                    is_dir: true,
                    location: EntryLocation::Fs(parent.to_path_buf()),
                });
            }
        }

        let mut dir_entries: Vec<DirEntry> = dirs
            .into_iter()
            .map(|d| DirEntry {
                name: d.clone(),
                is_dir: true,
                location: EntryLocation::Zip {
                    archive_path: archive_path.to_path_buf(),
                    inner_path: if cwd.is_empty() {
                        d
                    } else {
                        format!("{}/{}", cwd.trim_end_matches('/'), d)
                    },
                },
            })
            .collect();

        let mut file_entries: Vec<DirEntry> = files
            .into_iter()
            .map(|f| DirEntry {
                name: f.clone(),
                is_dir: false,
                location: EntryLocation::Zip {
                    archive_path: archive_path.to_path_buf(),
                    inner_path: if cwd.is_empty() {
                        f
                    } else {
                        format!("{}/{}", cwd.trim_end_matches('/'), f)
                    },
                },
            })
            .collect();

        dir_entries.sort_by(|a, b| a.name.cmp(&b.name));
        file_entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries.extend(dir_entries);
        entries.extend(file_entries);

        Ok(entries)
    }

    fn panel(&self, which: ActivePanel) -> &PanelState {
        match which {
            ActivePanel::Left => &self.left_panel,
            ActivePanel::Right => &self.right_panel,
        }
    }

    fn panel_mut(&mut self, which: ActivePanel) -> &mut PanelState {
        match which {
            ActivePanel::Left => &mut self.left_panel,
            ActivePanel::Right => &mut self.right_panel,
        }
    }

    fn get_active_panel(&self) -> &PanelState {
        self.panel(self.active_panel.clone())
    }

    fn get_active_panel_mut(&mut self) -> &mut PanelState {
        self.panel_mut(self.active_panel.clone())
    }

    fn select_entry(&mut self, index: usize) {
        let panel = self.get_active_panel_mut();
        if index < panel.entries.len() {
            panel.selected_index = index;
        } else {
            log::error!("Unable to select entry at index {}", index);
        }
    }

    fn open_selected(&mut self, cx: &mut gpui::Context<Self>) {
        let p = self.active_panel.clone();
        let panel = self.get_active_panel();
        if panel.entries.is_empty() {
            return;
        }
        let entry = &panel.entries[panel.selected_index];
        match &entry.location {
            EntryLocation::Fs(path) => {
                if entry.is_dir {
                    self.load_fs_directory(path.clone(), p, cx);
                } else if is_zip_path(path) {
                    self.load_zip_directory(path.clone(), "".to_string(), p, cx);
                }
            }
            EntryLocation::Zip {
                archive_path,
                inner_path,
            } => {
                if entry.is_dir {
                    self.load_zip_directory(archive_path.clone(), inner_path.clone(), p, cx);
                }
            }
        }
    }

    fn switch_panel(&mut self) {
        self.active_panel = match self.active_panel {
            ActivePanel::Left => ActivePanel::Right,
            ActivePanel::Right => ActivePanel::Left,
        };
    }

    fn toggle_preview(&mut self) {
        if self.preview.is_some() {
            self.preview = None;
            return;
        }
        let panel = self.get_active_panel();
        if panel.entries.is_empty() {
            return;
        }
        let entry = &panel.entries[panel.selected_index];
        match &entry.location {
            EntryLocation::Fs(path) => {
                if entry.is_dir {
                    return;
                }
                if is_image_path(path) {
                    self.preview = Some(PreviewContent::Image(Arc::from(path.clone())));
                } else if is_text_path(path) {
                    let content = read_text_preview(path, 2 * 1024 * 1024);
                    if let Ok(text) = content {
                        self.preview = Some(PreviewContent::Text(text));
                    }
                }
            }
            EntryLocation::Zip { .. } => {
                // For now, skip preview for zipped entries for simplicity
            }
        }
    }

    fn enqueue_copy_selected(&mut self) {
        let src = {
            let p = self.get_active_panel();
            if p.entries.is_empty() {
                return;
            }
            match &p.entries[p.selected_index].location {
                EntryLocation::Fs(path) => path.clone(),
                EntryLocation::Zip { .. } => {
                    // Skip copy for zip-internal entries for now
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
                    // Can't copy into zip for now
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
}

fn is_zip_path(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()),
        Some(ext) if ext == "zip"
    )
}

fn is_image_path(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()),
        Some(ext) if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp")
    )
}

fn is_text_path(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()),
        Some(ext)
            if matches!(
                ext.as_str(),
                "txt" | "md" | "json" | "toml" | "yaml" | "yml" | "rs" | "log" | "ini" | "csv"
            )
    )
}

fn read_text_preview(path: &Path, max_bytes: usize) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::new();
    file.by_ref().take(max_bytes as u64).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// Views
struct FileManagerView {
    model: gpui::Entity<FileSystemModel>,
    focus_handle: gpui::FocusHandle,
}

impl gpui::Focusable for FileManagerView {
    fn focus_handle(&self, _app: &gpui::App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EventEmitter<gpui::DismissEvent> for FileManagerView {}

impl gpui::Render for FileManagerView {
    #[profiling::function]
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        gpui::div()
            .flex()
            .flex_row()
            .size_full()
            .child(self.render_panel(ActivePanel::Left, cx))
            .child(
                gpui::div()
                    .w(gpui::px(2.0))
                    .bg(gpui::Rgba {
                        r: 0.2,
                        g: 0.2,
                        b: 0.2,
                        a: 1.0,
                    })
                    .h_full(),
            )
            .child(self.render_panel(ActivePanel::Right, cx))
            .child(self.render_preview(cx))
            .key_context("parent")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(
                |this: &mut Self,
                 event: &gpui::KeyDownEvent,
                 _window,
                 cx: &mut gpui::Context<Self>| {
                    let key = event.keystroke.key.as_str();
                    let handled = match key {
                        "tab" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                model.switch_panel();
                            });
                            true
                        }
                        "enter" => {
                            this.model.update(cx, |model: &mut FileSystemModel, cx| {
                                model.open_selected(cx);
                            });
                            true
                        }
                        "down" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                let panel = model.get_active_panel();
                                if panel.selected_index + 1 < panel.entries.len() {
                                    model.select_entry(panel.selected_index + 1);
                                }
                            });
                            true
                        }
                        "up" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                let panel = model.get_active_panel();
                                if panel.selected_index > 0 {
                                    model.select_entry(panel.selected_index - 1);
                                }
                            });
                            true
                        }
                        "f3" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                model.toggle_preview();
                            });
                            true
                        }
                        "escape" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                model.preview = None;
                            });
                            true
                        }
                        "f5" => {
                            this.model.update(cx, |model: &mut FileSystemModel, _| {
                                model.enqueue_copy_selected();
                            });
                            true
                        }
                        _ => false,
                    };

                    if handled {
                        cx.notify();
                        cx.stop_propagation();
                    }
                },
            ))
    }
}

impl FileManagerView {
    #[profiling::function]
    fn render_panel(
        &self,
        panel_side: ActivePanel,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let model = self.model.read(cx);
        let panel = match panel_side {
            ActivePanel::Left => &model.left_panel,
            ActivePanel::Right => &model.right_panel,
        };
        let is_active = model.active_panel == panel_side;
        let target_is_left = matches!(panel_side, ActivePanel::Left);

        let path_display = match &panel.mode {
            PanelMode::Fs => panel.current_path.to_string_lossy().into_owned(),
            PanelMode::Zip { archive_path, cwd } => {
                if cwd.is_empty() {
                    format!("{}::zip:/", archive_path.to_string_lossy())
                } else {
                    format!("{}::zip:/{}", archive_path.to_string_lossy(), cwd)
                }
            }
        };

        let mut file_list =
            gpui::div()
                .flex_1()
                .p_2()
                .h_full()
                .w_full()
                .children(panel.entries.iter().enumerate().map(|(index, entry)| {
                let is_selected = panel.selected_index == index;
                let is_directory = entry.is_dir;

                gpui::div()
                    .py_1()
                    .px_2()
                    .w_full()
                    .bg(if is_selected {
                        gpui::Hsla::from(gpui::Rgba {
                            r: 0.2,
                            g: 0.4,
                            b: 0.7,
                            a: 1.0,
                        })
                    } else {
                        gpui::transparent_black()
                    })
                    .text_color(if is_selected {
                        gpui::white()
                    } else {
                        gpui::Hsla::from(gpui::Rgba {
                            r: 0.9,
                            g: 0.9,
                            b: 0.9,
                            a: 1.0,
                        })
                    })
                    .font_weight(if is_directory {
                        gpui::FontWeight::BOLD
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .child(format!(
                        "{}{}",
                        if is_directory { "📁 " } else { "📄 " },
                        entry.name
                    ))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(
                            move |this: &mut Self,
                                  event: &gpui::MouseDownEvent,
                                  _window,
                                  cx: &mut gpui::Context<Self>| {
                                if !is_active {
                                    this.model.update(cx, |m: &mut FileSystemModel, _| {
                                        m.active_panel = if target_is_left {
                                            ActivePanel::Left
                                        } else {
                                            ActivePanel::Right
                                        };
                                    });
                                }

                                this.model.update(cx, move |m: &mut FileSystemModel, cx| {
                                    m.select_entry(index);
                                    if event.click_count > 1 {
                                        m.open_selected(cx);
                                    }
                                });
                                cx.notify();
                            },
                        ),
                    )
            }));
        file_list.style().overflow = gpui::PointRefinement {
            x: Some(gpui::Overflow::Hidden),
            y: Some(gpui::Overflow::Scroll),
        };
        file_list.style().scrollbar_width = Some(gpui::px(30.0).into());

        gpui::div()
            .flex()
            .flex_col()
            .size_full()
            .border_1()
            .border_color(if is_active {
                gpui::Hsla::from(gpui::Rgba {
                    r: 0.2,
                    g: 0.6,
                    b: 0.9,
                    a: 1.0,
                })
            } else {
                gpui::transparent_black()
            })
            .child(
                // Path header
                gpui::div()
                    .p_2()
                    .bg(gpui::Rgba {
                        r: 0.75,
                        g: 0.75,
                        b: 0.75,
                        a: 1.0,
                    })
                    .w_full()
                    .child(path_display),
            )
            .child(file_list)
    }

    fn render_preview(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        if let Some(preview) = &model.preview {
            let content = match preview {
                PreviewContent::Text(text) => {
                    let mut area = gpui::div().p_2().w_full().h_full().child(text.clone());
                    area.style().overflow = gpui::PointRefinement {
                        x: Some(gpui::Overflow::Hidden),
                        y: Some(gpui::Overflow::Scroll),
                    };
                    gpui::div().flex_1().p_2().child(area)
                }
                PreviewContent::Image(path) => gpui::div()
                    .flex_1()
                    .p_2()
                    .child(gpui::img(path.clone()).w_full().h_full()),
            };

            gpui::div()
                .w(gpui::px(420.0))
                .h_full()
                .border_l_1()
                .border_color(gpui::Hsla::from(gpui::Rgba {
                    r: 0.3,
                    g: 0.3,
                    b: 0.3,
                    a: 1.0,
                }))
                .bg(gpui::Hsla::from(gpui::Rgba {
                    r: 0.1,
                    g: 0.1,
                    b: 0.1,
                    a: 1.0,
                }))
                .flex()
                .flex_col()
                .child(
                    gpui::div()
                        .p_2()
                        .bg(gpui::Rgba {
                            r: 0.2,
                            g: 0.2,
                            b: 0.2,
                            a: 1.0,
                        })
                        .text_color(gpui::white())
                        .child("Preview (F3 to close, Esc to close)"),
                )
                .child(content)
        } else {
            // zero-width placeholder to keep layout simple
            gpui::div().w(gpui::px(0.0)).h_full()
        }
    }
}
