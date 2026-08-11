use super::*;

use std::path::PathBuf;

use language::DiagnosticSeverity;
use project::{Project, ProjectPath};
use settings::ShowDiagnostics;

/// Splits `path` into ancestor prefixes, root first: `a/b/c.rs` becomes `[a, a/b, a/b/c.rs]`.
fn breadcrumb_path_prefixes(path: &RelPath) -> Vec<&RelPath> {
    let mut prefixes: Vec<&RelPath> = path
        .ancestors()
        .filter(|prefix| !prefix.is_empty())
        .collect();
    prefixes.reverse();
    prefixes
}

pub(crate) fn breadcrumb_path_segments(
    worktree_id: WorktreeId,
    root_name: &str,
    path: &Arc<RelPath>,
    active_path: Option<Arc<RelPath>>,
    terminal_buffer_id: Option<BufferId>,
    active_segment: Option<&RelPath>,
    file_segment_active: bool,
) -> (Vec<HighlightedText>, Vec<Option<BreadcrumbSegmentTarget>>) {
    let mut labels = vec![HighlightedText {
        text: root_name.to_string().into(),
        highlights: vec![],
    }];
    let mut targets = vec![Some(BreadcrumbSegmentTarget::Directory {
        worktree_id,
        path: RelPath::empty().into_arc(),
        active_path: active_path.clone(),
        is_active_segment: active_segment == Some(RelPath::empty()),
    })];

    let prefixes = breadcrumb_path_prefixes(path);
    let last_prefix_index = prefixes.len().saturating_sub(1);
    for (prefix_index, prefix) in prefixes.iter().copied().enumerate() {
        let name = prefix.file_name().unwrap_or_else(|| prefix.as_unix_str());
        labels.push(HighlightedText {
            text: name.to_string().into(),
            highlights: vec![],
        });
        targets.push(Some(
            if prefix_index == last_prefix_index
                && let Some(buffer_id) = terminal_buffer_id
            {
                BreadcrumbSegmentTarget::Symbol {
                    buffer_id,
                    item: None,
                    is_active_segment: file_segment_active,
                }
            } else {
                BreadcrumbSegmentTarget::Directory {
                    worktree_id,
                    path: prefix.into_arc(),
                    active_path: active_path.clone(),
                    is_active_segment: active_segment == Some(prefix),
                }
            },
        ));
    }

    (labels, targets)
}

pub(crate) fn breadcrumb_segment_copy_path(
    target: &BreadcrumbSegmentTarget,
    worktree_abs_path: Option<PathBuf>,
    file_abs_path: Option<PathBuf>,
    symbol_line: Option<u32>,
) -> Option<String> {
    match target {
        BreadcrumbSegmentTarget::Directory { path, .. } => Some(
            worktree_abs_path?
                .join(path.as_std_path())
                .to_string_lossy()
                .into_owned(),
        ),
        BreadcrumbSegmentTarget::Symbol { item: None, .. } => {
            Some(file_abs_path?.to_string_lossy().into_owned())
        }
        BreadcrumbSegmentTarget::Symbol { item: Some(_), .. } => Some(format!(
            "{}:{}",
            file_abs_path?.to_string_lossy(),
            symbol_line?
        )),
    }
}

/// Deliberately exposes no breadcrumb-specific overrides yet; the bar always follows the panel.
#[derive(Clone, Copy, settings::RegisterSetting)]
pub struct BreadcrumbDirectoryListingSettings {
    pub sort_mode: settings::ProjectPanelSortMode,
    pub sort_order: settings::ProjectPanelSortOrder,
    pub hide_gitignore: bool,
    pub hide_hidden: bool,
    pub file_icons: bool,
    pub folder_icons: bool,
    pub git_status: bool,
    pub show_diagnostics: ShowDiagnostics,
    pub diagnostic_badges: bool,
    pub auto_fold_dirs: bool,
}

impl settings::Settings for BreadcrumbDirectoryListingSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let project_panel = content.project_panel.clone().unwrap();
        Self {
            sort_mode: project_panel.sort_mode.unwrap(),
            sort_order: project_panel.sort_order.unwrap(),
            hide_gitignore: project_panel.hide_gitignore.unwrap(),
            hide_hidden: project_panel.hide_hidden.unwrap(),
            file_icons: project_panel.file_icons.unwrap(),
            folder_icons: project_panel.folder_icons.unwrap(),
            git_status: project_panel.git_status.unwrap()
                && content
                    .git
                    .as_ref()
                    .unwrap()
                    .enabled
                    .unwrap()
                    .is_git_status_enabled(),
            show_diagnostics: project_panel.show_diagnostics.unwrap(),
            diagnostic_badges: project_panel.diagnostic_badges.unwrap(),
            auto_fold_dirs: project_panel.auto_fold_dirs.unwrap(),
        }
    }
}

/// Reads the aggregated per-path summary rather than walking diagnostics, since this runs on every render.
pub fn breadcrumb_diagnostic_severity(
    project: &Project,
    project_path: &ProjectPath,
    show_diagnostics: ShowDiagnostics,
    cx: &App,
) -> Option<DiagnosticSeverity> {
    if show_diagnostics == ShowDiagnostics::Off {
        return None;
    }
    let summary = project.diagnostic_summary_for_path(project_path, cx);
    if summary.error_count > 0 {
        Some(DiagnosticSeverity::ERROR)
    } else if show_diagnostics == ShowDiagnostics::All && summary.warning_count > 0 {
        Some(DiagnosticSeverity::WARNING)
    } else {
        None
    }
}

pub struct BreadcrumbDirectoryEntry {
    pub name: SharedString,
    pub path: Arc<RelPath>,
    pub is_dir: bool,
    pub is_ignored: bool,
    pub git_summary: GitSummary,
}

pub fn breadcrumb_directory_entries(
    project: &Entity<Project>,
    worktree: &Entity<project::Worktree>,
    path: &RelPath,
    cx: &App,
) -> Vec<BreadcrumbDirectoryEntry> {
    let settings = BreadcrumbDirectoryListingSettings::get_global(cx);
    let worktree_snapshot = worktree.read(cx).snapshot();
    let project_ref = project.read(cx);
    let repo_snapshots = project_ref.git_store().read(cx).display_repo_snapshots(cx);
    let mut entries = project::git_store::git_traversal::ChildEntriesGitIter::new(
        &repo_snapshots,
        &worktree_snapshot,
        path,
    )
    .filter(|entry| !settings.hide_gitignore || !entry.is_ignored)
    .filter(|entry| !settings.hide_hidden || !entry.is_hidden)
    .map(|entry| entry.to_owned())
    .collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        util::paths::compare_rel_paths_by(
            (&*a.path, a.is_file()),
            (&*b.path, b.is_file()),
            settings.sort_mode.into(),
            settings.sort_order.into(),
        )
    });

    entries
        .into_iter()
        .filter_map(|entry| {
            let name = entry.path.file_name()?.to_string();
            Some(BreadcrumbDirectoryEntry {
                name: name.into(),
                path: entry.path.clone(),
                is_dir: entry.is_dir(),
                is_ignored: entry.is_ignored,
                git_summary: entry.git_summary,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn sample_symbol_outline_item() -> OutlineItem<Anchor> {
        OutlineItem {
            depth: 0,
            range: Anchor::Min..Anchor::Max,
            selection_range: Anchor::Min..Anchor::Max,
            source_range_for_text: Anchor::Min..Anchor::Max,
            text: "method".into(),
            highlight_ranges: Vec::new(),
            name_ranges: Vec::new(),
            body_range: None,
            annotation_range: None,
        }
    }

    #[test]
    fn test_breadcrumb_segment_copy_path_per_segment_kind() {
        use util::path;
        use util::rel_path::rel_path;

        let directory = BreadcrumbSegmentTarget::Directory {
            worktree_id: WorktreeId::from_usize(0),
            path: rel_path("src/main").into_arc(),
            active_path: None,
            is_active_segment: false,
        };
        let copied = breadcrumb_segment_copy_path(
            &directory,
            Some(PathBuf::from(path!("/root"))),
            None,
            None,
        );
        assert_eq!(
            copied.as_deref(),
            Some(path!("/root/src/main")),
            "a directory segment joins the worktree root and the segment path"
        );

        let file_abs_path = Some(PathBuf::from(path!("/root/src/main/Foo.kt")));
        let file = BreadcrumbSegmentTarget::Symbol {
            buffer_id: BufferId::new(1).unwrap(),
            item: None,
            is_active_segment: false,
        };
        let copied = breadcrumb_segment_copy_path(&file, None, file_abs_path.clone(), None);
        assert_eq!(
            copied.as_deref(),
            Some(path!("/root/src/main/Foo.kt")),
            "the file segment is the file's absolute path"
        );

        let symbol = BreadcrumbSegmentTarget::Symbol {
            buffer_id: BufferId::new(1).unwrap(),
            item: Some(sample_symbol_outline_item()),
            is_active_segment: false,
        };
        let copied = breadcrumb_segment_copy_path(&symbol, None, file_abs_path, Some(42));
        assert_eq!(
            copied,
            Some(format!("{}:42", path!("/root/src/main/Foo.kt"))),
            "a symbol segment appends the line"
        );
    }

    #[test]
    fn test_breadcrumb_path_prefixes() {
        use util::rel_path::rel_path;

        assert_eq!(
            breadcrumb_path_prefixes(rel_path("a/b/c.rs")),
            vec![rel_path("a"), rel_path("a/b"), rel_path("a/b/c.rs")]
        );
        assert_eq!(
            breadcrumb_path_prefixes(rel_path("file.rs")),
            vec![rel_path("file.rs")]
        );
        assert_eq!(
            breadcrumb_path_prefixes(RelPath::empty()),
            Vec::<&RelPath>::new()
        );
    }

    #[test]
    fn test_breadcrumb_path_segments_nested() {
        use util::rel_path::rel_path;

        let worktree_id = WorktreeId::from_usize(0);
        let buffer_id = BufferId::new(1).unwrap();
        let path = rel_path("src/main/kotlin/Foo.kt").into_arc();

        let (labels, targets) = breadcrumb_path_segments(
            worktree_id,
            "my-project",
            &path,
            Some(path.clone()),
            Some(buffer_id),
            None,
            false,
        );

        assert_eq!(
            labels.iter().map(|l| l.text.as_ref()).collect::<Vec<_>>(),
            vec!["my-project", "src", "main", "kotlin", "Foo.kt"]
        );
        assert_eq!(targets.len(), labels.len());

        match targets[0].as_ref().unwrap() {
            BreadcrumbSegmentTarget::Directory {
                worktree_id: id,
                path,
                active_path,
                is_active_segment,
            } => {
                assert_eq!(*id, worktree_id);
                assert_eq!(path.as_unix_str(), "");
                assert_eq!(
                    active_path.as_deref(),
                    Some(rel_path("src/main/kotlin/Foo.kt"))
                );
                assert!(!is_active_segment);
            }
            other => panic!("expected root directory target, got {other:?}"),
        }

        for (index, expected_dir) in ["src", "src/main", "src/main/kotlin"]
            .into_iter()
            .enumerate()
        {
            match targets[index + 1].as_ref().unwrap() {
                BreadcrumbSegmentTarget::Directory { path, .. } => {
                    assert_eq!(path.as_unix_str(), expected_dir);
                }
                other => panic!("expected directory target, got {other:?}"),
            }
        }

        match targets.last().unwrap().as_ref().unwrap() {
            BreadcrumbSegmentTarget::Symbol {
                buffer_id: id,
                item,
                is_active_segment,
            } => {
                assert_eq!(*id, buffer_id);
                assert!(item.is_none());
                assert!(!is_active_segment);
            }
            other => panic!("expected symbol target for the file segment, got {other:?}"),
        }
    }

    #[test]
    fn test_breadcrumb_path_segments_top_level_file() {
        use util::rel_path::rel_path;

        let worktree_id = WorktreeId::from_usize(0);
        let buffer_id = BufferId::new(1).unwrap();
        let path = rel_path("Foo.kt").into_arc();

        let (labels, targets) = breadcrumb_path_segments(
            worktree_id,
            "my-project",
            &path,
            Some(path.clone()),
            Some(buffer_id),
            None,
            false,
        );

        assert_eq!(
            labels.iter().map(|l| l.text.as_ref()).collect::<Vec<_>>(),
            vec!["my-project", "Foo.kt"]
        );
        assert!(matches!(
            targets[0].as_ref().unwrap(),
            BreadcrumbSegmentTarget::Directory { .. }
        ));
        assert!(matches!(
            targets[1].as_ref().unwrap(),
            BreadcrumbSegmentTarget::Symbol { item: None, .. }
        ));
    }

    #[test]
    fn test_breadcrumb_path_segments_navigated_directory_marks_active_segment() {
        use util::rel_path::rel_path;

        let worktree_id = WorktreeId::from_usize(0);
        let path = rel_path("src/main").into_arc();

        let (labels, targets) = breadcrumb_path_segments(
            worktree_id,
            "ihavenever",
            &path,
            None,
            None,
            Some(rel_path("src/main")),
            false,
        );

        assert_eq!(
            labels.iter().map(|l| l.text.as_ref()).collect::<Vec<_>>(),
            vec!["ihavenever", "src", "main"]
        );

        let active_flags: Vec<bool> = targets
            .iter()
            .map(|target| match target.as_ref().unwrap() {
                BreadcrumbSegmentTarget::Directory {
                    is_active_segment, ..
                } => *is_active_segment,
                BreadcrumbSegmentTarget::Symbol { .. } => {
                    panic!("navigated directory path should have no symbol target")
                }
            })
            .collect();
        assert_eq!(active_flags, vec![false, false, true]);
    }

    #[test]
    fn test_breadcrumb_path_segments_drill_down_includes_root_and_lists_own_children() {
        use util::rel_path::rel_path;

        let worktree_id = WorktreeId::from_usize(0);
        let path = rel_path("src/main/Foo.kt").into_arc();

        let (labels, targets) = breadcrumb_path_segments(
            worktree_id,
            "my-project",
            &path,
            Some(path.clone()),
            None,
            None,
            false,
        );

        assert_eq!(
            labels.iter().map(|l| l.text.as_ref()).collect::<Vec<_>>(),
            vec!["my-project", "src", "main", "Foo.kt"]
        );

        let list_paths: Vec<String> = targets
            .iter()
            .map(|target| match target.as_ref().unwrap() {
                BreadcrumbSegmentTarget::Directory { path, .. } => path.as_unix_str().to_string(),
                BreadcrumbSegmentTarget::Symbol { .. } => "<symbol>".to_string(),
            })
            .collect();
        assert_eq!(list_paths, vec!["", "src", "src/main", "src/main/Foo.kt"]);
    }

    #[gpui::test]
    async fn test_breadcrumb_directory_entries_sorts_like_project_panel(cx: &mut TestAppContext) {
        use crate::editor_tests::init_test;
        use project::{FakeFs, Project};
        use serde_json::json;
        use settings::SettingsStore;
        use util::path;

        init_test(cx, |_| {});

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "Apple": { "leaf.txt": "" },
                "banana.txt": "",
                "Cherry.txt": "",
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree = project.update(cx, |project, cx| project.worktrees(cx).next().unwrap());
        cx.run_until_parked();

        let entries =
            cx.update(|cx| breadcrumb_directory_entries(&project, &worktree, RelPath::empty(), cx));
        assert_eq!(
            entries.iter().map(|e| e.name.as_ref()).collect::<Vec<_>>(),
            vec!["Apple", "banana.txt", "Cherry.txt"],
        );

        // Reuses `compare_rel_paths_by`, so ordering tracks `project_panel.sort_mode`/`sort_order`.
        cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    let project_panel = settings.project_panel.get_or_insert_default();
                    project_panel.sort_mode = Some(settings::ProjectPanelSortMode::FilesFirst);
                    project_panel.sort_order = Some(settings::ProjectPanelSortOrder::Unicode);
                });
            });
        });
        let entries =
            cx.update(|cx| breadcrumb_directory_entries(&project, &worktree, RelPath::empty(), cx));
        assert_eq!(
            entries.iter().map(|e| e.name.as_ref()).collect::<Vec<_>>(),
            vec!["Cherry.txt", "banana.txt", "Apple"],
        );
    }

    #[gpui::test]
    async fn test_breadcrumb_directory_entries_honors_hide_gitignore_setting(
        cx: &mut TestAppContext,
    ) {
        use crate::editor_tests::init_test;
        use project::{FakeFs, Project};
        use serde_json::json;
        use settings::SettingsStore;
        use util::path;

        init_test(cx, |_| {});

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                ".gitignore": "ignored.txt",
                "kept.txt": "",
                "ignored.txt": "",
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree = project.update(cx, |project, cx| project.worktrees(cx).next().unwrap());
        cx.run_until_parked();

        let entries =
            cx.update(|cx| breadcrumb_directory_entries(&project, &worktree, RelPath::empty(), cx));
        let ignored_entry = entries
            .iter()
            .find(|entry| entry.name.as_ref() == "ignored.txt")
            .expect("gitignored entry is shown, not hidden, by default");
        assert!(ignored_entry.is_ignored);

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
        let entries =
            cx.update(|cx| breadcrumb_directory_entries(&project, &worktree, RelPath::empty(), cx));
        assert_eq!(
            entries.iter().map(|e| e.name.as_ref()).collect::<Vec<_>>(),
            vec![".gitignore", "kept.txt"],
            "hide_gitignore should drop the ignored entry entirely, not just dim it",
        );
    }

    #[gpui::test]
    async fn test_breadcrumb_directory_entries_honors_hide_hidden_setting(cx: &mut TestAppContext) {
        use crate::editor_tests::init_test;
        use project::{FakeFs, Project};
        use serde_json::json;
        use settings::SettingsStore;
        use util::path;

        init_test(cx, |_| {});

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                ".hidden": "",
                "kept.txt": "",
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree = project.update(cx, |project, cx| project.worktrees(cx).next().unwrap());
        cx.run_until_parked();

        let entries =
            cx.update(|cx| breadcrumb_directory_entries(&project, &worktree, RelPath::empty(), cx));
        assert_eq!(
            entries.iter().map(|e| e.name.as_ref()).collect::<Vec<_>>(),
            vec![".hidden", "kept.txt"],
            "hidden entry is shown by default"
        );

        cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.project_panel.get_or_insert_default().hide_hidden = Some(true);
                });
            });
        });
        let entries =
            cx.update(|cx| breadcrumb_directory_entries(&project, &worktree, RelPath::empty(), cx));
        assert_eq!(
            entries.iter().map(|e| e.name.as_ref()).collect::<Vec<_>>(),
            vec!["kept.txt"],
            "hide_hidden should drop the hidden entry entirely, not just dim it",
        );
    }
}
