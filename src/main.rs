use gpui::prelude::*;
use std::{fs, path};

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let cur_dir = std::env::current_dir()?;
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
                let fs_entity = app.new(|_| FileSystemModel {
                    left_panel: PanelState {
                        current_path: cur_dir.clone(),
                        selected_index: 0,
                        entries: Vec::new(),
                    },
                    right_panel: PanelState {
                        current_path: cur_dir,
                        selected_index: 0,
                        entries: Vec::new(),
                    },
                    active_panel: ActivePanel::Left,
                });

                // Load initial directories
                app.update_entity(&fs_entity, |model: &mut FileSystemModel, cx| {
                    model.load_directory(
                        model.left_panel.current_path.clone(),
                        ActivePanel::Left,
                        cx,
                    );
                    model.load_directory(
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

// Panel-related types
#[derive(Clone, PartialEq)]
enum ActivePanel {
    Left,
    Right,
}

struct PanelState {
    current_path: path::PathBuf,
    selected_index: usize,
    entries: Vec<DirEntry>,
}

struct DirEntry {
    name: String,
    path: path::PathBuf,
    is_dir: bool,
}

// Models
struct FileSystemModel {
    left_panel: PanelState,
    right_panel: PanelState,
    active_panel: ActivePanel,
}

impl FileSystemModel {
    #[profiling::function]
    fn load_directory(
        &mut self,
        path: path::PathBuf,
        active_panel: ActivePanel,
        _cx: &mut gpui::Context<Self>,
    ) {
        //TODO: consider reading the directory asynchronously
        let entries_result = Self::read_directory(&path);
        let panel_state = match active_panel {
            ActivePanel::Left => &mut self.left_panel,
            ActivePanel::Right => &mut self.right_panel,
        };

        match entries_result {
            Ok(entries) => {
                panel_state.current_path = path;
                panel_state.entries = entries;
                panel_state.selected_index = 0;
            }
            Err(e) => {
                // Handle error (could display in UI)
                eprintln!("Error loading directory: {}", e);
            }
        }
    }

    #[profiling::function]
    fn read_directory(path: &path::Path) -> anyhow::Result<Vec<DirEntry>> {
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
                path: entry.path(),
                is_dir,
            });
        }

        // Sort directories first, then files
        dir_entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

        // Add parent directory entry if not at root
        if path.parent().is_some() {
            entries.push(DirEntry {
                name: "..".to_string(),
                path: path.parent().unwrap().to_path_buf(),
                is_dir: true,
            });
        }

        entries.extend(dir_entries);

        Ok(entries)
    }

    fn navigate_to(
        &mut self,
        path: path::PathBuf,
        panel: ActivePanel,
        cx: &mut gpui::Context<Self>,
    ) {
        self.load_directory(path, panel, cx);
    }

    fn get_active_panel(&self) -> &PanelState {
        match self.active_panel {
            ActivePanel::Left => &self.left_panel,
            ActivePanel::Right => &self.right_panel,
        }
    }

    fn get_active_panel_mut(&mut self) -> &mut PanelState {
        match self.active_panel {
            ActivePanel::Left => &mut self.left_panel,
            ActivePanel::Right => &mut self.right_panel,
        }
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
        let panel = self.get_active_panel();
        let entry = &panel.entries[panel.selected_index];
        if entry.is_dir {
            let active_panel = self.active_panel.clone();
            self.navigate_to(entry.path.clone(), active_panel, cx);
        }
        // For files, you could implement a file viewer or other action
    }

    fn switch_panel(&mut self) {
        self.active_panel = match self.active_panel {
            ActivePanel::Left => ActivePanel::Right,
            ActivePanel::Right => ActivePanel::Left,
        };
    }
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
                    .w(gpui::Pixels(2.0))
                    .bg(gpui::Rgba {
                        r: 0.2,
                        g: 0.2,
                        b: 0.2,
                        a: 1.0,
                    })
                    .h_full(),
            )
            .child(self.render_panel(ActivePanel::Right, cx))
            .key_context("parent")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(
                |this: &mut Self,
                 event: &gpui::KeyDownEvent,
                 _window,
                 cx: &mut gpui::Context<Self>| {
                    let handled = match event.keystroke.key.as_str() {
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
        active_panel: ActivePanel,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let model = self.model.read(cx);
        let panel = match active_panel {
            ActivePanel::Left => &model.left_panel,
            ActivePanel::Right => &model.right_panel,
        };
        let is_active = model.active_panel == active_panel;
        let path_display = panel.current_path.to_string_lossy().into_owned();

        let mut file_list = gpui::div().flex_1().p_2().h_full().w_full().children(
            panel.entries.iter().enumerate().map(|(index, entry)| {
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
                                    this.model.update(cx, |model: &mut FileSystemModel, _| {
                                        model.switch_panel();
                                    });
                                };

                                this.model
                                    .update(cx, move |model: &mut FileSystemModel, cx| {
                                        model.select_entry(index);
                                        if event.click_count > 1 {
                                            model.open_selected(cx);
                                        }
                                    });
                                cx.notify();
                            },
                        ),
                    )
            }),
        );
        file_list.style().overflow = gpui::PointRefinement {
            x: Some(gpui::Overflow::Hidden),
            y: Some(gpui::Overflow::Scroll),
        };
        file_list.style().scrollbar_width = Some(30.0);

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
}
