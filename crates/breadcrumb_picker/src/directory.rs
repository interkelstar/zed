use std::rc::Rc;
use std::sync::Arc;

use editor::{
    BreadcrumbDirectoryEntry, BreadcrumbDirectoryListingSettings, Editor,
    ErasedBreadcrumbPopoverHandle, breadcrumb_diagnostic_severity, breadcrumb_directory_entries,
};
use fuzzy::{StringMatch, StringMatchCandidate};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, MouseButton, ParentElement, Styled, Task,
    WeakEntity, Window, div, rems,
};
use language::DiagnosticSeverity;
use picker::{Picker, PickerDelegate, PickerEditorPosition};
use project::{Project, ProjectPath, WorktreeId};
use settings::{Settings, ShowDiagnostics};
use ui::{
    ButtonLike, ButtonSize, ButtonStyle, Color, HighlightedLabel, Icon, IconSize, ListItem,
    ListItemSpacing, PopoverMenu, PopoverMenuHandle, prelude::*,
};
use util::ResultExt;
use util::rel_path::RelPath;
use workspace::Workspace;

use crate::MAX_BREADCRUMB_MENU_ENTRIES;

// Guards against a pathologically deep chain or a symlink cycle.
const MAX_BREADCRUMB_DESCENT_DEPTH: usize = 64;

fn descend_single_child_directories(
    start: Arc<RelPath>,
    mut child_entries: impl FnMut(&RelPath) -> Vec<(Arc<RelPath>, bool)>,
) -> Arc<RelPath> {
    let mut current = start;
    for _ in 0..MAX_BREADCRUMB_DESCENT_DEPTH {
        let children = child_entries(&current);
        let [(only_child_path, only_child_is_dir)] = children.as_slice() else {
            return current;
        };
        if !only_child_is_dir {
            return current;
        }
        current = only_child_path.clone();
    }
    current
}

/// Applies the listing's own hidden-entry predicates directly to the worktree snapshot and stops after two entries.
fn breadcrumb_directory_children(
    worktree: &Entity<project::Worktree>,
    path: &RelPath,
    cx: &App,
) -> Vec<(Arc<RelPath>, bool)> {
    let settings = BreadcrumbDirectoryListingSettings::get_global(cx);
    worktree
        .read(cx)
        .snapshot()
        .child_entries(path)
        .filter(|entry| !settings.hide_gitignore || !entry.is_ignored)
        .filter(|entry| !settings.hide_hidden || !entry.is_hidden)
        .take(2)
        .map(|entry| (entry.path.clone(), entry.is_dir()))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreadcrumbEntryIconSource {
    File,
    Folder,
    Chevron,
    None,
}

fn breadcrumb_entry_label_color(
    entry: &BreadcrumbDirectoryEntry,
    git_status_enabled: bool,
    is_active_file: bool,
) -> Color {
    let git_summary = if git_status_enabled {
        entry.git_summary
    } else {
        Default::default()
    };
    editor::items::entry_git_aware_label_color(git_summary, entry.is_ignored, is_active_file)
}

fn breadcrumb_entry_icon_color(diagnostic_severity: Option<DiagnosticSeverity>) -> Color {
    editor::items::entry_diagnostic_aware_icon_decoration_and_color(diagnostic_severity)
        .map(|(_, color)| color)
        .unwrap_or(Color::Muted)
}

fn breadcrumb_entry_icon_source(
    is_dir: bool,
    show_file_icons: bool,
    show_folder_icons: bool,
) -> BreadcrumbEntryIconSource {
    if is_dir {
        if show_folder_icons {
            BreadcrumbEntryIconSource::Folder
        } else {
            BreadcrumbEntryIconSource::Chevron
        }
    } else if show_file_icons {
        BreadcrumbEntryIconSource::File
    } else {
        BreadcrumbEntryIconSource::None
    }
}

/// The directory dropdown's contents. Choosing a directory navigates into it; a file opens it.
pub struct BreadcrumbDirectoryDelegate {
    editor: WeakEntity<Editor>,
    workspace: WeakEntity<Workspace>,
    worktree_id: WorktreeId,
    current_path: Arc<RelPath>,
    active_path: Option<Arc<RelPath>>,
    entries: Vec<BreadcrumbDirectoryEntry>,
    /// Rebuilt only alongside `entries`; a typed query clones this `Rc`, an empty one still copies up to the cap.
    candidates: Rc<[StringMatchCandidate]>,
    matches: Vec<StringMatch>,
    query: String,
    selected_index: usize,
    /// Whether any row draws an icon; reserving the column for none would indent the list.
    show_icons: bool,
    _expand_task: gpui::Task<()>,
}

pub type BreadcrumbDirectoryPicker = Picker<BreadcrumbDirectoryDelegate>;

/// Newtype avoiding an orphan-rule conflict for `ErasedBreadcrumbPopoverHandle`.
pub(crate) struct DirectoryPopoverHandle(pub PopoverMenuHandle<BreadcrumbDirectoryPicker>);

impl ErasedBreadcrumbPopoverHandle for DirectoryPopoverHandle {
    fn hide(&self, cx: &mut App) {
        self.0.hide(cx);
    }

    fn show(&self, window: &mut Window, cx: &mut App) {
        self.0.show(window, cx);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl BreadcrumbDirectoryDelegate {
    fn picker(
        editor: WeakEntity<Editor>,
        workspace: WeakEntity<Workspace>,
        worktree_id: WorktreeId,
        current_path: Arc<RelPath>,
        active_path: Option<Arc<RelPath>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<BreadcrumbDirectoryPicker> {
        cx.new(|cx| {
            let mut delegate = Self {
                editor,
                workspace,
                worktree_id,
                current_path,
                active_path,
                entries: Vec::new(),
                candidates: Vec::new().into(),
                matches: Vec::new(),
                query: String::new(),
                selected_index: 0,
                show_icons: false,
                _expand_task: gpui::Task::ready(()),
            };
            // `Picker::uniform_list` below runs an initial `update_matches("")`, which now only filters.
            delegate.reload_entries(cx);
            let mut picker = Picker::uniform_list(delegate, window, cx)
                .popover()
                .show_scrollbar(true)
                // Narrower than the picker default, which is sized for modals.
                .initial_width(rems(15.));
            picker.delegate.expand_current_path(window, cx);
            picker.delegate.select_active_path();
            picker
        })
    }

    fn project(&self, cx: &App) -> Option<Entity<Project>> {
        Some(self.workspace.upgrade()?.read(cx).project().clone())
    }

    fn worktree(&self, cx: &App) -> Option<Entity<project::Worktree>> {
        self.project(cx)?
            .read(cx)
            .worktree_for_id(self.worktree_id, cx)
    }

    fn reload_entries(&mut self, cx: &App) {
        let (Some(project), Some(worktree)) = (self.project(cx), self.worktree(cx)) else {
            self.entries = Vec::new();
            self.candidates = Vec::new().into();
            self.matches.clear();
            return;
        };
        self.entries = breadcrumb_directory_entries(&project, &worktree, &self.current_path, cx);
        self.candidates = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| StringMatchCandidate::new(index, &entry.name))
            .collect::<Vec<_>>()
            .into();
        // `matches` holds candidate_id/positions into the entries just replaced above.
        self.matches.clear();

        let settings = BreadcrumbDirectoryListingSettings::get_global(cx);
        self.show_icons = self.entries.iter().any(|entry| {
            breadcrumb_entry_icon_source(entry.is_dir, settings.file_icons, settings.folder_icons)
                != BreadcrumbEntryIconSource::None
        });
    }

    fn select_active_path(&mut self) {
        let Some(active_path) = self.active_path.as_ref() else {
            return;
        };
        self.selected_index = self
            .matches
            .iter()
            .position(|entry_match| {
                self.entries
                    .get(entry_match.candidate_id)
                    .is_some_and(|entry| active_path.starts_with(&entry.path))
            })
            .unwrap_or(0);
    }

    /// Gitignored directories are never scanned proactively; without this the dropdown is empty.
    fn expand_current_path(
        &mut self,
        window: &mut Window,
        cx: &mut Context<BreadcrumbDirectoryPicker>,
    ) {
        let Some(project) = self.project(cx) else {
            return;
        };
        let Some(entry_id) = self
            .worktree(cx)
            .and_then(|worktree| worktree.read(cx).entry_for_path(&self.current_path))
            .map(|entry| entry.id)
        else {
            return;
        };
        let Some(expand) = project.update(cx, |project, cx| {
            project.expand_entry(self.worktree_id, entry_id, cx)
        }) else {
            return;
        };

        self._expand_task = cx.spawn_in(window, async move |picker, cx| {
            expand.await.log_err();
            picker
                .update_in(cx, |picker, window, cx| {
                    picker.delegate.reload_entries(cx);
                    picker.refresh(window, cx);
                })
                .ok();
        });
    }

    fn entry_at(&self, index: usize) -> Option<&BreadcrumbDirectoryEntry> {
        self.entries.get(self.matches.get(index)?.candidate_id)
    }

    /// Resolved per row, not for the whole listing, so opening a huge directory stays cheap.
    fn row_diagnostic_severity(
        &self,
        entry: &BreadcrumbDirectoryEntry,
        show_diagnostics: ShowDiagnostics,
        cx: &App,
    ) -> Option<DiagnosticSeverity> {
        if entry.is_dir {
            return None;
        }
        let project = self.project(cx)?;
        breadcrumb_diagnostic_severity(
            project.read(cx),
            &ProjectPath {
                worktree_id: self.worktree_id,
                path: entry.path.clone(),
            },
            show_diagnostics,
            cx,
        )
    }
}

impl PickerDelegate for BreadcrumbDirectoryDelegate {
    type ListItem = AnyElement;

    fn name() -> &'static str {
        "breadcrumb directory picker"
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<BreadcrumbDirectoryPicker>,
    ) {
        self.selected_index = index;
        cx.notify();
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Search this folder…".into()
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        Some(if self.entries.is_empty() {
            "Empty directory".into()
        } else {
            "No matches".into()
        })
    }

    fn editor_position(&self) -> PickerEditorPosition {
        PickerEditorPosition::End
    }

    fn extra_key_context(&self) -> Option<&'static str> {
        Some("BreadcrumbPicker")
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        cx: &mut Context<BreadcrumbDirectoryPicker>,
    ) -> Task<()> {
        self.query = query.clone();

        if query.is_empty() {
            let active_candidate_id = self.active_path.as_ref().and_then(|active_path| {
                self.entries
                    .iter()
                    .position(|entry| active_path.starts_with(&entry.path))
            });
            self.matches = crate::cap_empty_query_matches(
                &self.candidates,
                active_candidate_id,
                MAX_BREADCRUMB_MENU_ENTRIES,
            );
            self.select_active_path();
            cx.notify();
            return Task::ready(());
        }

        let candidates = self.candidates.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |picker, cx| {
            let matches = fuzzy::match_strings(
                &candidates,
                &query,
                false,
                true,
                MAX_BREADCRUMB_MENU_ENTRIES,
                &Default::default(),
                executor,
            )
            .await;
            picker
                .update(cx, |picker, cx| {
                    picker.delegate.matches = matches;
                    picker.delegate.selected_index = 0;
                    cx.notify();
                })
                .ok();
        })
    }

    fn confirm(
        &mut self,
        _secondary: bool,
        window: &mut Window,
        cx: &mut Context<BreadcrumbDirectoryPicker>,
    ) {
        let Some(entry) = self.entry_at(self.selected_index) else {
            return;
        };
        let entry_path = entry.path.clone();

        if !entry.is_dir {
            if let Some(workspace) = self.workspace.upgrade() {
                workspace.update(cx, |workspace, cx| {
                    workspace
                        .open_path(
                            ProjectPath {
                                worktree_id: self.worktree_id,
                                path: entry_path,
                            },
                            None,
                            true,
                            window,
                            cx,
                        )
                        .detach_and_log_err(cx);
                });
            }
            cx.emit(DismissEvent);
            return;
        }

        let Some(worktree) = self.worktree(cx) else {
            return;
        };
        let auto_fold_dirs = BreadcrumbDirectoryListingSettings::get_global(cx).auto_fold_dirs;
        let resolved_path = if auto_fold_dirs {
            descend_single_child_directories(entry_path, |path| {
                breadcrumb_directory_children(&worktree, path, cx)
            })
        } else {
            entry_path
        };

        // `current_path` isn't updated in place; the popover reopens under the resolved segment.
        if let Some(editor) = self.editor.upgrade() {
            editor.update(cx, |editor, cx| {
                editor.navigate_breadcrumb_to(self.worktree_id, resolved_path, window, cx);
            });
        }
    }

    // Some rather than None when handled: None lets the keystroke fall through to cursor movement in the query editor.
    fn select_child(
        &mut self,
        window: &mut Window,
        cx: &mut Context<BreadcrumbDirectoryPicker>,
    ) -> Option<String> {
        if !self.query.is_empty() {
            return None;
        }
        if self.entry_at(self.selected_index)?.is_dir {
            self.confirm(false, window, cx);
            return Some(String::new());
        }
        None
    }

    fn select_parent(
        &mut self,
        window: &mut Window,
        cx: &mut Context<BreadcrumbDirectoryPicker>,
    ) -> Option<String> {
        if !self.query.is_empty() {
            return None;
        }
        let parent = self.current_path.parent()?.into_arc();
        if let Some(editor) = self.editor.upgrade() {
            editor.update(cx, |editor, cx| {
                editor.navigate_breadcrumb_to(self.worktree_id, parent, window, cx);
            });
        }
        Some(String::new())
    }

    fn dismissed(&mut self, _window: &mut Window, _cx: &mut Context<BreadcrumbDirectoryPicker>) {}

    fn render_match(
        &self,
        index: usize,
        selected: bool,
        _window: &mut Window,
        cx: &mut Context<BreadcrumbDirectoryPicker>,
    ) -> Option<Self::ListItem> {
        let entry = self.entry_at(index)?;
        let listing_settings = BreadcrumbDirectoryListingSettings::get_global(cx);

        let leads_to_active_path = entry.is_dir
            && self
                .active_path
                .as_ref()
                .is_some_and(|active_path| active_path.starts_with(&entry.path));
        let is_active_file =
            !entry.is_dir && self.active_path.as_deref() == Some(entry.path.as_ref());

        let icon_path = match breadcrumb_entry_icon_source(
            entry.is_dir,
            listing_settings.file_icons,
            listing_settings.folder_icons,
        ) {
            BreadcrumbEntryIconSource::File => {
                file_icons::FileIcons::get_icon(entry.path.as_std_path(), cx)
            }
            BreadcrumbEntryIconSource::Folder => file_icons::FileIcons::get_folder_icon(
                leads_to_active_path,
                entry.path.as_std_path(),
                cx,
            ),
            BreadcrumbEntryIconSource::Chevron => {
                file_icons::FileIcons::get_chevron_icon(false, cx)
            }
            BreadcrumbEntryIconSource::None => None,
        };
        let diagnostic_severity =
            self.row_diagnostic_severity(entry, listing_settings.show_diagnostics, cx);
        let icon = icon_path.map(Icon::from_path).map(|icon| {
            icon.color(breadcrumb_entry_icon_color(diagnostic_severity))
                .size(IconSize::Small)
                .into_any_element()
        });

        let label_color =
            breadcrumb_entry_label_color(entry, listing_settings.git_status, is_active_file);

        Some(
            ListItem::new(SharedString::from(format!(
                "breadcrumb-directory-entry-{index}"
            )))
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .when(self.show_icons, |this| {
                this.start_slot(
                    div()
                        .flex_none()
                        .size(IconSize::Small.rems())
                        .children(icon),
                )
            })
            .child(
                HighlightedLabel::new(entry.name.clone(), self.matches[index].positions.clone())
                    .color(label_color),
            )
            .into_any_element(),
        )
    }
}

pub(crate) fn render_breadcrumb_directory_segment(
    editor: WeakEntity<Editor>,
    workspace: WeakEntity<Workspace>,
    worktree_id: WorktreeId,
    path: Arc<RelPath>,
    active_path: Option<Arc<RelPath>>,
    is_active_segment: bool,
    shared_popover_handle: Rc<dyn ErasedBreadcrumbPopoverHandle>,
    label: gpui::AnyElement,
    index: usize,
) -> gpui::AnyElement {
    let trigger = ButtonLike::new(("breadcrumb-directory", index))
        .style(ButtonStyle::Transparent)
        .size(ButtonSize::None)
        .height(rems_from_px(22.).into())
        .child(label);

    // Only the active segment carries the handle `Editor::navigate_breadcrumb_to` reopens through.
    let popover_handle = if is_active_segment {
        shared_popover_handle
            .as_any()
            .downcast_ref::<DirectoryPopoverHandle>()
            .map(|handle| handle.0.clone())
            .unwrap_or_default()
    } else {
        PopoverMenuHandle::default()
    };

    let reveal_workspace = workspace.clone();
    let reveal_path = path.clone();

    let menu = PopoverMenu::new(("breadcrumb-directory-menu", index))
        .with_handle(popover_handle)
        .trigger_with_tooltip(
            trigger,
            ui::Tooltip::text("Double-Click to Reveal in Project Panel"),
        )
        .menu(move |window, cx| {
            let workspace_entity = workspace.upgrade()?;
            workspace_entity
                .read(cx)
                .project()
                .read(cx)
                .worktree_for_id(worktree_id, cx)?;

            if let Some(editor_entity) = editor.upgrade() {
                editor_entity.update(cx, |editor, cx| {
                    editor.open_breadcrumb_navigation(worktree_id, path.clone(), cx);
                });
            }

            let picker = BreadcrumbDirectoryDelegate::picker(
                editor.clone(),
                workspace.clone(),
                worktree_id,
                path.clone(),
                active_path.clone(),
                window,
                cx,
            );
            if let Some(editor_entity) = editor.upgrade() {
                editor_entity.update(cx, |editor, cx| {
                    editor.watch_breadcrumb_dismissal(&picker, worktree_id, path.clone(), cx);
                });
            }
            Some(picker)
        });

    // Capture-phase mouse down, not click: the popover's dismiss handler swallows the second click.
    div()
        .capture_any_mouse_down(move |event, _, cx| {
            if event.button != MouseButton::Left || event.click_count < 2 {
                return;
            }
            reveal_breadcrumb_directory_in_project_panel(
                &reveal_workspace,
                worktree_id,
                &reveal_path,
                cx,
            );
        })
        .child(menu)
        .into_any_element()
}

fn reveal_breadcrumb_directory_in_project_panel(
    workspace: &WeakEntity<Workspace>,
    worktree_id: WorktreeId,
    path: &RelPath,
    cx: &mut App,
) {
    let Some(workspace) = workspace.upgrade() else {
        return;
    };
    let project = workspace.read(cx).project().clone();
    let Some(entry_id) = project
        .read(cx)
        .entry_for_path(
            &ProjectPath {
                worktree_id,
                path: path.into(),
            },
            cx,
        )
        .map(|entry| entry.id)
    else {
        return;
    };
    project.update(cx, |_, cx| {
        cx.emit(project::Event::ActivateProjectPanel);
        cx.emit(project::Event::RevealInProjectPanel(entry_id));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use editor::Editor;
    use gpui::{Focusable, Render, TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use workspace::Workspace;

    use crate::test_support::{Harness, bind_drill_navigation_keymap};

    fn confirm_breadcrumb_row(
        picker: &Entity<BreadcrumbDirectoryPicker>,
        path: &str,
        cx: &mut VisualTestContext,
    ) {
        use util::rel_path::rel_path;
        picker.update_in(cx, |picker, window, cx| {
            let index = picker
                .delegate
                .matches
                .iter()
                .position(|entry_match| {
                    picker.delegate.entries[entry_match.candidate_id]
                        .path
                        .as_ref()
                        == rel_path(path)
                })
                .expect("row is listed");
            picker.delegate.selected_index = index;
            picker.delegate.confirm(false, window, cx);
        });
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let _app_state = workspace::AppState::test(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
    }

    fn test_entry(git_summary: git::status::GitSummary) -> BreadcrumbDirectoryEntry {
        BreadcrumbDirectoryEntry {
            name: "file.txt".into(),
            path: util::rel_path::rel_path("file.txt").into_arc(),
            is_dir: false,
            is_ignored: false,
            git_summary,
        }
    }

    #[test]
    fn test_breadcrumb_entry_label_color_honors_git_status_setting() {
        let entry = test_entry(git::status::GitSummary::UNTRACKED);

        assert_eq!(
            breadcrumb_entry_label_color(&entry, true, false),
            Color::Created
        );

        assert_eq!(
            breadcrumb_entry_label_color(&entry, false, false),
            Color::Muted
        );
    }

    #[test]
    fn test_breadcrumb_entry_icon_color_follows_diagnostic_severity() {
        assert_eq!(breadcrumb_entry_icon_color(None), Color::Muted);
        assert_eq!(
            breadcrumb_entry_icon_color(Some(DiagnosticSeverity::WARNING)),
            Color::Warning
        );
        assert_eq!(
            breadcrumb_entry_icon_color(Some(DiagnosticSeverity::ERROR)),
            Color::Error
        );
    }

    #[test]
    fn test_breadcrumb_entry_icon_source() {
        assert_eq!(
            breadcrumb_entry_icon_source(true, true, true),
            BreadcrumbEntryIconSource::Folder
        );
        assert_eq!(
            breadcrumb_entry_icon_source(true, false, true),
            BreadcrumbEntryIconSource::Folder
        );
        assert_eq!(
            breadcrumb_entry_icon_source(true, true, false),
            BreadcrumbEntryIconSource::Chevron
        );
        assert_eq!(
            breadcrumb_entry_icon_source(true, false, false),
            BreadcrumbEntryIconSource::Chevron
        );
        assert_eq!(
            breadcrumb_entry_icon_source(false, true, true),
            BreadcrumbEntryIconSource::File
        );
        assert_eq!(
            breadcrumb_entry_icon_source(false, true, false),
            BreadcrumbEntryIconSource::File
        );
        assert_eq!(
            breadcrumb_entry_icon_source(false, false, true),
            BreadcrumbEntryIconSource::None
        );
        assert_eq!(
            breadcrumb_entry_icon_source(false, false, false),
            BreadcrumbEntryIconSource::None
        );
    }

    #[test]
    fn test_descend_single_child_directories_stops_at_fork() {
        use util::rel_path::rel_path;

        let tree: collections::HashMap<&str, Vec<(&str, bool)>> =
            collections::HashMap::from_iter([
                ("a", vec![("a/b", true)]),
                ("a/b", vec![("a/b/c", true), ("a/b/d", true)]),
            ]);

        let result = descend_single_child_directories(rel_path("a").into_arc(), |path| {
            tree.get(path.as_unix_str())
                .into_iter()
                .flatten()
                .map(|(child, is_dir)| (rel_path(child).into_arc(), *is_dir))
                .collect()
        });

        assert_eq!(result, rel_path("a/b").into_arc());
    }

    #[test]
    fn test_descend_single_child_directories_stops_short_of_lone_file() {
        use util::rel_path::rel_path;

        let tree: collections::HashMap<&str, Vec<(&str, bool)>> = collections::HashMap::from_iter(
            [("repository", vec![("repository/Repositories.kt", false)])],
        );

        let result = descend_single_child_directories(rel_path("repository").into_arc(), |path| {
            tree.get(path.as_unix_str())
                .into_iter()
                .flatten()
                .map(|(child, is_dir)| (rel_path(child).into_arc(), *is_dir))
                .collect()
        });

        assert_eq!(result, rel_path("repository").into_arc());
    }

    #[test]
    fn test_descend_single_child_directories_caps_depth() {
        use util::rel_path::rel_path;

        // Simulates a symlink cycle; the cap must stop the walk instead of looping forever.
        let result = descend_single_child_directories(rel_path("a").into_arc(), |path| {
            vec![(
                rel_path(&format!("{}/x", path.as_unix_str())).into_arc(),
                true,
            )]
        });

        assert_eq!(
            result.as_unix_str().matches('/').count(),
            MAX_BREADCRUMB_DESCENT_DEPTH
        );
    }

    #[test]
    fn test_descend_single_child_directories_stops_at_empty_directory() {
        use util::rel_path::rel_path;

        let result = descend_single_child_directories(rel_path("empty").into_arc(), |_| Vec::new());

        assert_eq!(result, rel_path("empty").into_arc());
    }

    #[gpui::test]
    async fn test_choosing_breadcrumb_directory_row_does_not_double_lease_browser(
        cx: &mut TestAppContext,
    ) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use serde_json::json;
        use util::path;

        // `PopoverMenu` only wires up during a real layout pass; needs a real `Render` root.
        struct Harness {
            handle: PopoverMenuHandle<BreadcrumbDirectoryPicker>,
            editor: Entity<Editor>,
            workspace: WeakEntity<Workspace>,
            worktree_id: WorktreeId,
            captured_browser: Rc<RefCell<Option<Entity<BreadcrumbDirectoryPicker>>>>,
        }

        impl Render for Harness {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                let editor = self.editor.downgrade();
                let workspace = self.workspace.clone();
                let worktree_id = self.worktree_id;
                let captured_browser = self.captured_browser.clone();
                PopoverMenu::new("test-breadcrumb-directory-menu")
                    .with_handle(self.handle.clone())
                    .trigger(ButtonLike::new("trigger").child(div()))
                    .menu(move |window, cx| {
                        if let Some(editor_entity) = editor.upgrade() {
                            editor_entity.update(cx, |editor, cx| {
                                editor.open_breadcrumb_navigation(
                                    worktree_id,
                                    RelPath::empty().into(),
                                    cx,
                                );
                            });
                        }
                        let browser = BreadcrumbDirectoryDelegate::picker(
                            editor.clone(),
                            workspace.clone(),
                            worktree_id,
                            RelPath::empty().into(),
                            None,
                            window,
                            cx,
                        );
                        *captured_browser.borrow_mut() = Some(browser.clone());
                        Some(browser)
                    })
            }
        }

        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "dir_a": {
                    "child1.txt": "",
                    "child2.txt": "",
                },
                "file.txt": "",
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let captured_browser: Rc<RefCell<Option<Entity<BreadcrumbDirectoryPicker>>>> =
            Rc::default();

        let harness_window = cx.add_window(|window, cx| {
            let editor = cx.new(|cx| build_editor(buffer, window, cx));
            let handle = editor
                .read(cx)
                .breadcrumb_popover_handle()
                .expect("breadcrumb_picker::init registered the renderers")
                .as_any()
                .downcast_ref::<DirectoryPopoverHandle>()
                .expect("the registered handle constructor is this crate's own")
                .0
                .clone();
            Harness {
                handle,
                editor,
                workspace: workspace.downgrade(),
                worktree_id,
                captured_browser: captured_browser.clone(),
            }
        });
        let editor = harness_window
            .read_with(cx, |harness, _| harness.editor.clone())
            .unwrap();
        let handle = harness_window
            .read_with(cx, |harness, _| harness.handle.clone())
            .unwrap();
        let cx = &mut VisualTestContext::from_window(*harness_window, cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.update(|window, cx| handle.show(window, cx));
        let browser = captured_browser.borrow().clone().expect("popover opened");
        assert!(handle.is_deployed());
        editor.read_with(cx, |editor, _| {
            assert!(
                editor.breadcrumb_navigation().is_some(),
                "opening the popover marked this segment active"
            );
        });

        // Choosing while `browser` is still leased by this `update` call is the actual repro.
        confirm_breadcrumb_row(&browser, "dir_a", cx);

        editor.read_with(cx, |editor, _| {
            let navigation = editor
                .breadcrumb_navigation()
                .expect("navigate_breadcrumb_to set a session");
            assert_eq!(navigation.active_path.as_unix_str(), "dir_a");
            assert!(navigation.navigated);
            assert!(
                editor.breadcrumb_reanchoring(),
                "re-anchor is still in flight, the popover isn't back open yet"
            );
        });
        assert!(
            !handle.is_deployed(),
            "the pre-navigation popover was dismissed synchronously by the defer"
        );

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.reanchor_breadcrumb_popover(window, cx);
            });
        });

        editor.read_with(cx, |editor, _| {
            assert!(
                !editor.breadcrumb_reanchoring(),
                "re-anchor finishes within a few frames"
            );
        });
        assert!(
            handle.is_deployed(),
            "the popover reopened under the resolved directory's own segment"
        );
    }

    #[gpui::test]
    async fn test_breadcrumb_directory_picker_navigates_from_the_keyboard(cx: &mut TestAppContext) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use serde_json::json;
        use util::path;

        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "alpha": { "one.txt": "", "two.txt": "" },
                "beta": { "three.txt": "" },
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let harness_window = cx.add_window(|window, cx| {
            let editor = cx.new(|cx| build_editor(buffer, window, cx));
            let picker = BreadcrumbDirectoryDelegate::picker(
                editor.downgrade(),
                workspace.downgrade(),
                worktree_id,
                RelPath::empty().into(),
                None,
                window,
                cx,
            );
            Harness { picker, editor }
        });
        let (picker, editor) = harness_window
            .read_with(cx, |harness, _| {
                (harness.picker.clone(), harness.editor.clone())
            })
            .unwrap();
        let cx = &mut VisualTestContext::from_window(*harness_window, cx);
        cx.run_until_parked();

        picker.update_in(cx, |picker, window, cx| {
            window.focus(&picker.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        cx.dispatch_action(menu::SelectNext);
        picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker
                    .delegate
                    .entry_at(picker.delegate.selected_index)
                    .map(|entry| entry.name.as_ref()),
                Some("beta"),
            );
        });

        cx.dispatch_action(menu::Confirm);
        cx.run_until_parked();

        editor.read_with(cx, |editor, _| {
            let navigation = editor
                .breadcrumb_navigation()
                .expect("confirming a directory row navigates the bar into it");
            assert_eq!(navigation.active_path.as_unix_str(), "beta");
            assert!(navigation.navigated);
        });
    }

    #[gpui::test]
    async fn test_revealing_a_breadcrumb_directory_emits_for_the_project_panel(
        cx: &mut TestAppContext,
    ) {
        use project::{FakeFs, Project};
        use serde_json::json;
        use std::sync::Arc as StdArc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use util::{path, rel_path::rel_path};

        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/root"), json!({ "alpha": { "one.txt": "" } }))
            .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });
        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let revealed = StdArc::new(AtomicUsize::new(0));
        let activated = StdArc::new(AtomicUsize::new(0));
        let _subscription = cx.update(|cx| {
            cx.subscribe(&project, {
                let revealed = revealed.clone();
                let activated = activated.clone();
                move |_, event, _| match event {
                    project::Event::RevealInProjectPanel(_) => {
                        revealed.fetch_add(1, Ordering::AcqRel);
                    }
                    project::Event::ActivateProjectPanel => {
                        activated.fetch_add(1, Ordering::AcqRel);
                    }
                    _ => {}
                }
            })
        });

        cx.update(|cx| {
            reveal_breadcrumb_directory_in_project_panel(
                &workspace.downgrade(),
                worktree_id,
                rel_path("alpha"),
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(revealed.load(Ordering::Acquire), 1);
        assert_eq!(
            activated.load(Ordering::Acquire),
            1,
            "a closed panel has to open, not just have its selection moved"
        );

        cx.update(|cx| {
            reveal_breadcrumb_directory_in_project_panel(
                &workspace.downgrade(),
                worktree_id,
                rel_path("nope"),
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(
            revealed.load(Ordering::Acquire),
            1,
            "a path with no entry reveals nothing rather than panicking"
        );
        assert_eq!(activated.load(Ordering::Acquire), 1);
    }

    #[gpui::test]
    async fn test_breadcrumb_directory_browser_expands_nested_gitignored_directories(
        cx: &mut TestAppContext,
    ) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use serde_json::json;
        use util::{path, rel_path::rel_path};

        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                ".gitignore": "ignored_dir\n",
                "ignored_dir": { "nested": { "file.txt": "" } },
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree = project.update(cx, |project, cx| project.worktrees(cx).next().unwrap());
        let worktree_id = worktree.read_with(cx, |worktree, _| worktree.id());
        cx.run_until_parked();

        let entries = cx.update(|cx| {
            breadcrumb_directory_entries(&project, &worktree, rel_path("ignored_dir"), cx)
        });
        assert!(
            entries.is_empty(),
            "nothing under a gitignored directory is scanned until something asks for it"
        );

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();
        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let editor_window = cx.add_window(|window, cx| build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap();
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let open_dropdown_at = |path: &'static str, cx: &mut VisualTestContext| {
            let browser = editor_window
                .update(cx, |_, window, cx| {
                    BreadcrumbDirectoryDelegate::picker(
                        editor.downgrade(),
                        workspace.downgrade(),
                        worktree_id,
                        rel_path(path).into_arc(),
                        None,
                        window,
                        cx,
                    )
                })
                .unwrap();
            cx.run_until_parked();
            let entries = cx.update(|_, cx| {
                breadcrumb_directory_entries(&project, &worktree, rel_path(path), cx)
            });
            drop(browser);
            entries
                .into_iter()
                .map(|entry| entry.name.to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            open_dropdown_at("ignored_dir", cx),
            vec!["nested".to_string()],
            "opening the dropdown scans one level into the gitignored directory"
        );
        assert_eq!(
            open_dropdown_at("ignored_dir/nested", cx),
            vec!["file.txt".to_string()],
            "and the level below it once that one is opened too"
        );
    }

    #[gpui::test]
    async fn test_breadcrumb_directory_browser_choose_descends_single_child_directories(
        cx: &mut TestAppContext,
    ) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use serde_json::json;
        use util::path;

        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "a": { "b": { "c.txt": "" } },
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let editor_window = cx.add_window(|window, cx| build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap();
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let browser = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbDirectoryDelegate::picker(
                    editor.downgrade(),
                    workspace.downgrade(),
                    worktree_id,
                    RelPath::empty().into(),
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();
        confirm_breadcrumb_row(&browser, "a", cx);
        editor.read_with(cx, |editor, _| {
            assert_eq!(
                editor
                    .breadcrumb_navigation()
                    .expect("navigate_breadcrumb_to set a session")
                    .active_path
                    .as_unix_str(),
                "a/b",
            );
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_directory_browser_choose_respects_auto_fold_dirs_off(
        cx: &mut TestAppContext,
    ) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use serde_json::json;
        use settings::SettingsStore;
        use util::path;

        init_test(cx);
        cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings
                        .project_panel
                        .get_or_insert_default()
                        .auto_fold_dirs = Some(false);
                });
            });
        });

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "a": { "b": { "c.txt": "" } },
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let editor_window = cx.add_window(|window, cx| build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap();
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let browser = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbDirectoryDelegate::picker(
                    editor.downgrade(),
                    workspace.downgrade(),
                    worktree_id,
                    RelPath::empty().into(),
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();
        confirm_breadcrumb_row(&browser, "a", cx);
        editor.read_with(cx, |editor, _| {
            assert_eq!(
                editor
                    .breadcrumb_navigation()
                    .expect("navigate_breadcrumb_to set a session")
                    .active_path
                    .as_unix_str(),
                "a",
                "with auto_fold_dirs off, confirm must land on the chosen directory itself"
            );
        });
    }

    #[gpui::test]
    async fn test_auto_fold_does_not_descend_into_a_directory_the_settings_hide(
        cx: &mut TestAppContext,
    ) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use serde_json::json;
        use settings::SettingsStore;
        use util::path;

        init_test(cx);
        cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings
                        .project_panel
                        .get_or_insert_default()
                        .hide_gitignore = Some(true);
                });
            });
        });

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                ".gitignore": "b\n",
                "a": { "b": { "c.txt": "" } },
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let editor_window = cx.add_window(|window, cx| build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap();
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let browser = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbDirectoryDelegate::picker(
                    editor.downgrade(),
                    workspace.downgrade(),
                    worktree_id,
                    RelPath::empty().into(),
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();
        confirm_breadcrumb_row(&browser, "a", cx);
        editor.read_with(cx, |editor, _| {
            assert_eq!(
                editor
                    .breadcrumb_navigation()
                    .expect("navigate_breadcrumb_to set a session")
                    .active_path
                    .as_unix_str(),
                "a",
                "b is gitignored and hidden, so the listing shows a as a leaf; auto-fold must \
                 not land somewhere the listing would never have shown"
            );
        });
    }

    #[gpui::test]
    async fn test_select_parent_and_child_only_drill_with_an_empty_query(cx: &mut TestAppContext) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use serde_json::json;
        use util::path;

        init_test(cx);
        bind_drill_navigation_keymap(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "alpha": { "beta": { "child.txt": "" }, "one.txt": "" },
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let harness_window = cx.add_window(|window, cx| {
            let editor = cx.new(|cx| build_editor(buffer, window, cx));
            let picker = BreadcrumbDirectoryDelegate::picker(
                editor.downgrade(),
                workspace.downgrade(),
                worktree_id,
                util::rel_path::rel_path("alpha").into_arc(),
                None,
                window,
                cx,
            );
            Harness { picker, editor }
        });
        let (picker, editor) = harness_window
            .read_with(cx, |harness, _| {
                (harness.picker.clone(), harness.editor.clone())
            })
            .unwrap();
        let cx = &mut VisualTestContext::from_window(*harness_window, cx);
        cx.run_until_parked();

        picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker.delegate.entry_at(0).map(|entry| entry.name.as_ref()),
                Some("beta"),
                "directories sort before files"
            );
        });

        picker.update_in(cx, |picker, window, cx| {
            window.focus(&picker.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("right");
        cx.run_until_parked();
        editor.read_with(cx, |editor, _| {
            assert!(
                editor.breadcrumb_navigation().is_some(),
                "an empty query lets right drill into the selected directory"
            );
        });

        cx.simulate_keystrokes("b e t a");
        cx.run_until_parked();
        picker.update(cx, |picker, cx| {
            assert_eq!(picker.query(cx), "beta");
        });

        // A non-empty query leaves left/right for the caret: a swallowed key would leave the
        // typed letter appended at the end instead of landing where the caret actually is.
        cx.simulate_keystrokes("left");
        cx.simulate_keystrokes("z");
        cx.run_until_parked();
        picker.update(cx, |picker, cx| {
            assert_eq!(picker.query(cx), "betza");
        });

        cx.simulate_keystrokes("right");
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        picker.update(cx, |picker, cx| {
            assert_eq!(picker.query(cx), "betzay");
        });
    }

    #[gpui::test]
    async fn test_select_parent_with_an_empty_query_steps_out_to_the_parent_directory(
        cx: &mut TestAppContext,
    ) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use serde_json::json;
        use util::path;

        init_test(cx);
        bind_drill_navigation_keymap(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "alpha": { "beta": { "child.txt": "" } },
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let harness_window = cx.add_window(|window, cx| {
            let editor = cx.new(|cx| build_editor(buffer, window, cx));
            let picker = BreadcrumbDirectoryDelegate::picker(
                editor.downgrade(),
                workspace.downgrade(),
                worktree_id,
                util::rel_path::rel_path("alpha/beta").into_arc(),
                None,
                window,
                cx,
            );
            Harness { picker, editor }
        });
        let (picker, editor) = harness_window
            .read_with(cx, |harness, _| {
                (harness.picker.clone(), harness.editor.clone())
            })
            .unwrap();
        let cx = &mut VisualTestContext::from_window(*harness_window, cx);
        cx.run_until_parked();

        picker.update_in(cx, |picker, window, cx| {
            window.focus(&picker.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("left");
        cx.run_until_parked();

        editor.read_with(cx, |editor, _| {
            let navigation = editor
                .breadcrumb_navigation()
                .expect("an empty query lets left call select_parent, which navigates");
            assert_eq!(
                navigation.active_path.as_unix_str(),
                "alpha",
                "left steps out to the parent of the directory the picker opened at"
            );
            assert!(navigation.navigated);
        });
    }

    #[gpui::test]
    async fn test_update_matches_does_not_relist_entries(cx: &mut TestAppContext) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use serde_json::json;
        use util::path;

        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "alpha": { "one.txt": "" },
            }),
        )
        .await;
        let project = Project::test(fs.clone(), [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let editor_window = cx.add_window(|window, cx| build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap();
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let picker = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbDirectoryDelegate::picker(
                    editor.downgrade(),
                    workspace.downgrade(),
                    worktree_id,
                    util::rel_path::rel_path("alpha").into_arc(),
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();
        cx.run_until_parked();

        let entries_before = picker.read_with(cx, |picker, _| picker.delegate.entries.len());
        assert_eq!(entries_before, 1, "just one.txt at the start");

        fs.insert_file(path!("/root/alpha/two.txt"), Default::default())
            .await;
        cx.run_until_parked();

        picker
            .update_in(cx, |picker, window, cx| {
                picker
                    .delegate
                    .update_matches("one".to_string(), window, cx)
            })
            .await;

        picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker.delegate.entries.len(),
                entries_before,
                "a keystroke must filter the cached listing rather than rescan the directory"
            );
        });
    }

    #[gpui::test]
    async fn test_update_matches_reuses_cached_candidates(cx: &mut TestAppContext) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use serde_json::json;
        use util::path;

        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "alpha": { "one.txt": "", "two.txt": "" },
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let editor_window = cx.add_window(|window, cx| build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap();
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let picker = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbDirectoryDelegate::picker(
                    editor.downgrade(),
                    workspace.downgrade(),
                    worktree_id,
                    util::rel_path::rel_path("alpha").into_arc(),
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();
        cx.run_until_parked();

        let candidates_before =
            picker.read_with(cx, |picker, _| picker.delegate.candidates.clone());

        picker
            .update_in(cx, |picker, window, cx| {
                picker
                    .delegate
                    .update_matches("one".to_string(), window, cx)
            })
            .await;
        picker
            .update_in(cx, |picker, window, cx| {
                picker.delegate.update_matches("on".to_string(), window, cx)
            })
            .await;

        picker.read_with(cx, |picker, _| {
            assert!(
                std::rc::Rc::ptr_eq(&candidates_before, &picker.delegate.candidates),
                "successive keystrokes must reuse the same candidate list, not rebuild it"
            );
        });
    }

    #[gpui::test]
    async fn test_reload_entries_clears_stale_matches(cx: &mut TestAppContext) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use serde_json::json;
        use util::path;

        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "alpha": { "one.txt": "", "two.txt": "" },
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let editor_window = cx.add_window(|window, cx| build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap();
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let picker = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbDirectoryDelegate::picker(
                    editor.downgrade(),
                    workspace.downgrade(),
                    worktree_id,
                    util::rel_path::rel_path("alpha").into_arc(),
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();
        cx.run_until_parked();

        picker
            .update_in(cx, |picker, window, cx| {
                picker
                    .delegate
                    .update_matches("one".to_string(), window, cx)
            })
            .await;
        picker.read_with(cx, |picker, _| {
            assert_eq!(picker.delegate.matches.len(), 1, "query narrows to one.txt");
        });

        picker.update_in(cx, |picker, _window, cx| {
            picker.delegate.reload_entries(cx);
        });

        picker.read_with(cx, |picker, _| {
            assert!(
                picker.delegate.matches.is_empty(),
                "reload must drop matches whose candidate ids point into the entries just replaced"
            );
        });
    }

    #[gpui::test]
    async fn test_update_matches_caps_the_empty_query_display_and_keeps_the_active_entry(
        cx: &mut TestAppContext,
    ) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use project::{FakeFs, Project};
        use util::path;

        init_test(cx);

        let entry_count = MAX_BREADCRUMB_MENU_ENTRIES + 50;
        let mut alpha = serde_json::Map::new();
        for index in 0..entry_count {
            alpha.insert(
                format!("file_{index:04}.txt"),
                serde_json::Value::String(String::new()),
            );
        }
        let mut root = serde_json::Map::new();
        root.insert("alpha".to_string(), serde_json::Value::Object(alpha));

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/root"), serde_json::Value::Object(root))
            .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let editor_window = cx.add_window(|window, cx| build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap();
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        // The highest-numbered file sorts last, well past the cap.
        let active_path =
            util::rel_path::rel_path(&format!("alpha/file_{:04}.txt", entry_count - 1)).into_arc();

        let picker = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbDirectoryDelegate::picker(
                    editor.downgrade(),
                    workspace.downgrade(),
                    worktree_id,
                    util::rel_path::rel_path("alpha").into_arc(),
                    Some(active_path.clone()),
                    window,
                    cx,
                )
            })
            .unwrap();
        cx.run_until_parked();

        picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker.delegate.matches.len(),
                MAX_BREADCRUMB_MENU_ENTRIES,
                "the empty-query display is capped the same way a typed filter is"
            );
            assert!(
                picker.delegate.matches.iter().any(|entry_match| {
                    picker.delegate.entries[entry_match.candidate_id].path == active_path
                }),
                "the active entry stays among the displayed rows even though it sorts past the cap"
            );
            let selected = picker
                .delegate
                .entry_at(picker.delegate.selected_index)
                .expect("a row is selected");
            assert_eq!(
                selected.path, active_path,
                "the active entry is preselected"
            );
        });
    }

    #[gpui::test]
    async fn test_row_diagnostic_severity_is_resolved_per_row_not_at_listing_time(
        cx: &mut TestAppContext,
    ) {
        use editor::MultiBuffer;
        use editor::test::build_editor;
        use language::{Diagnostic, DiagnosticEntry, DiagnosticSourceKind};
        use lsp::{DiagnosticSeverity as LspDiagnosticSeverity, LanguageServerId};
        use project::{FakeFs, Project};
        use serde_json::json;
        use std::path::Path;
        use text::{PointUtf16, Unclipped};
        use util::path;

        init_test(cx);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "dir": {},
                "flagged.txt": "",
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();

        let buffer = cx.new(|cx| language::Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let editor_window = cx.add_window(|window, cx| build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap();
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let picker = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbDirectoryDelegate::picker(
                    editor.downgrade(),
                    workspace.downgrade(),
                    worktree_id,
                    RelPath::empty().into(),
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();
        cx.run_until_parked();

        picker.read_with(cx, |picker, cx| {
            let entry = picker
                .delegate
                .entries
                .iter()
                .find(|entry| entry.name.as_ref() == "flagged.txt")
                .expect("flagged.txt is listed");
            assert_eq!(
                picker
                    .delegate
                    .row_diagnostic_severity(entry, ShowDiagnostics::All, cx),
                None,
                "nothing reported yet"
            );
        });

        let lsp_store = project.read_with(cx, |project, _| project.lsp_store());
        lsp_store.update(cx, |lsp_store, cx| {
            lsp_store
                .update_diagnostic_entries(
                    LanguageServerId(0),
                    Path::new(path!("/root/flagged.txt")).to_owned(),
                    None,
                    None,
                    vec![DiagnosticEntry {
                        range: Unclipped(PointUtf16::new(0, 0))..Unclipped(PointUtf16::new(0, 1)),
                        diagnostic: Diagnostic {
                            severity: LspDiagnosticSeverity::ERROR,
                            is_primary: true,
                            message: "boom".to_string(),
                            source_kind: DiagnosticSourceKind::Pushed,
                            ..Diagnostic::default()
                        },
                    }],
                    cx,
                )
                .unwrap();
        });
        cx.run_until_parked();

        // No `reload_entries` call between the reads: the row still picks up the new diagnostic.
        picker.read_with(cx, |picker, cx| {
            let entry = picker
                .delegate
                .entries
                .iter()
                .find(|entry| entry.name.as_ref() == "flagged.txt")
                .expect("flagged.txt is listed");
            assert_eq!(
                picker
                    .delegate
                    .row_diagnostic_severity(entry, ShowDiagnostics::All, cx),
                Some(DiagnosticSeverity::ERROR),
            );
            assert_eq!(
                picker
                    .delegate
                    .row_diagnostic_severity(entry, ShowDiagnostics::Off, cx),
                None,
                "off suppresses diagnostics regardless of what is reported"
            );

            let dir_entry = picker
                .delegate
                .entries
                .iter()
                .find(|entry| entry.name.as_ref() == "dir")
                .expect("dir is listed");
            assert_eq!(
                picker
                    .delegate
                    .row_diagnostic_severity(dir_entry, ShowDiagnostics::All, cx),
                None,
                "directories never carry a severity"
            );
        });
    }
}
