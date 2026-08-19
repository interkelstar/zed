mod directory;
mod symbol;

use std::rc::Rc;

use editor::{
    BREADCRUMB_PICKER_RENDERERS, BreadcrumbPickerRenderers, ErasedBreadcrumbPopoverHandle,
};
use fuzzy::{StringMatch, StringMatchCandidate};
use gpui::{AnyElement, App, Context, Task, Window};
use picker::{Picker, PickerDelegate};
use ui::{ButtonLike, ButtonSize, ButtonStyle, PopoverMenuHandle, prelude::*};

pub(crate) const MAX_BREADCRUMB_MENU_ENTRIES: usize = 200;

/// Newtype avoiding an orphan-rule conflict for `ErasedBreadcrumbPopoverHandle`.
pub(crate) struct BreadcrumbPopoverHandle<D: PickerDelegate>(pub PopoverMenuHandle<Picker<D>>);

impl<D: PickerDelegate> ErasedBreadcrumbPopoverHandle for BreadcrumbPopoverHandle<D> {
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

/// The filter state both delegates embed and forward their `PickerDelegate` accessors to.
pub(crate) struct BreadcrumbPickerCore {
    /// Rebuilt only alongside the backing listing; a typed query clones this `Rc`, an empty one still copies up to the cap.
    pub candidates: Rc<[StringMatchCandidate]>,
    pub matches: Vec<StringMatch>,
    pub query: String,
    pub selected_index: usize,
}

impl Default for BreadcrumbPickerCore {
    fn default() -> Self {
        Self {
            candidates: Vec::new().into(),
            matches: Vec::new(),
            query: String::new(),
            selected_index: 0,
        }
    }
}

pub(crate) trait BreadcrumbPickerDelegate: PickerDelegate {
    type SelectionKey;

    fn core(&self) -> &BreadcrumbPickerCore;
    fn core_mut(&mut self) -> &mut BreadcrumbPickerCore;
    /// Whether the candidate is the entry the popover's own segment represents.
    fn is_current_candidate(&self, candidate_id: usize) -> bool;
    fn selection_key(&self, candidate_id: usize) -> Option<Self::SelectionKey>;
    fn matches_selection_key(
        &self,
        candidate_id: usize,
        selection_key: &Self::SelectionKey,
    ) -> bool;
    fn pending_selection_key(&self) -> &Option<Self::SelectionKey>;
    fn pending_selection_key_mut(&mut self) -> &mut Option<Self::SelectionKey>;
}

/// Remembers the selected row by identity; must run before the listing the key reads is swapped.
pub(crate) fn record_pending_selection(delegate: &mut impl BreadcrumbPickerDelegate) {
    let core = delegate.core();
    let selected_candidate_id = core
        .matches
        .get(core.selected_index)
        .map(|entry_match| entry_match.candidate_id);
    let selection_key =
        selected_candidate_id.and_then(|candidate_id| delegate.selection_key(candidate_id));
    *delegate.pending_selection_key_mut() = selection_key;
}

fn pending_selection_index<D: BreadcrumbPickerDelegate>(
    delegate: &D,
    matches: &[StringMatch],
) -> Option<usize> {
    let selection_key = delegate.pending_selection_key().as_ref()?;
    matches.iter().position(|entry_match| {
        delegate.matches_selection_key(entry_match.candidate_id, selection_key)
    })
}

/// Caps `matches` like a typed filter, preselecting the current entry and keeping it displayed even past the cap.
pub(crate) fn apply_empty_query_matches(delegate: &mut impl BreadcrumbPickerDelegate) {
    // Both delegates build candidates with `new(index, ..)`: ids double as listing indices.
    let keep_candidate_id = delegate
        .core()
        .candidates
        .iter()
        .map(|candidate| candidate.id)
        .find(|&candidate_id| delegate.is_current_candidate(candidate_id));
    let matches = cap_empty_query_matches(
        &delegate.core().candidates,
        keep_candidate_id,
        MAX_BREADCRUMB_MENU_ENTRIES,
    );
    let selected_index = pending_selection_index(delegate, &matches)
        .or_else(|| {
            matches
                .iter()
                .position(|entry_match| delegate.is_current_candidate(entry_match.candidate_id))
        })
        .unwrap_or(0);
    let core = delegate.core_mut();
    core.matches = matches;
    core.selected_index = selected_index;
}

pub(crate) fn update_picker_matches<D: BreadcrumbPickerDelegate>(
    delegate: &mut D,
    query: String,
    cx: &mut Context<Picker<D>>,
) -> Task<()> {
    // A newly typed query must land on its best match, not on the row a rebuild wanted back.
    if delegate.core().query != query {
        *delegate.pending_selection_key_mut() = None;
    }
    delegate.core_mut().query = query.clone();

    if query.is_empty() {
        apply_empty_query_matches(delegate);
        cx.notify();
        return Task::ready(());
    }

    let candidates = delegate.core().candidates.clone();
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
                let selected_index =
                    pending_selection_index(&picker.delegate, &matches).unwrap_or(0);
                let core = picker.delegate.core_mut();
                core.matches = matches;
                core.selected_index = selected_index;
                cx.notify();
            })
            .ok();
    })
}

/// What [`segment_trigger`] adds around a label, horizontally: `ButtonSize::None` pads a pixel a side.
pub(crate) const SEGMENT_TRIGGER_PADDING: Pixels = px(2.);

pub(crate) fn segment_trigger(id: &'static str, index: usize, label: AnyElement) -> ButtonLike {
    ButtonLike::new((id, index))
        .style(ButtonStyle::Transparent)
        .size(ButtonSize::None)
        .height(rems_from_px(22.).into())
        .child(label)
}

/// Only the active segment carries the handle the editor's navigate calls reopen through.
pub(crate) fn segment_popover_handle<D: PickerDelegate>(
    is_active_segment: bool,
    shared_popover_handle: Rc<dyn ErasedBreadcrumbPopoverHandle>,
) -> PopoverMenuHandle<Picker<D>> {
    if is_active_segment {
        shared_popover_handle
            .as_any()
            .downcast_ref::<BreadcrumbPopoverHandle<D>>()
            .map(|handle| handle.0.clone())
            .unwrap_or_default()
    } else {
        PopoverMenuHandle::default()
    }
}

/// Caps the empty-query listing like a typed filter, keeping `keep_candidate_id` displayed even if it would otherwise sort past the cap.
pub(crate) fn cap_empty_query_matches(
    candidates: &[StringMatchCandidate],
    keep_candidate_id: Option<usize>,
    cap: usize,
) -> Vec<StringMatch> {
    let to_match = |candidate: &StringMatchCandidate| StringMatch {
        candidate_id: candidate.id,
        string: candidate.string.clone(),
        positions: Vec::new(),
        score: 0.,
    };

    if candidates.len() <= cap {
        return candidates.iter().map(to_match).collect();
    }

    let keep_candidate = keep_candidate_id
        .and_then(|keep_candidate_id| {
            candidates
                .iter()
                .find(|candidate| candidate.id == keep_candidate_id)
        })
        .filter(|keep_candidate| {
            candidates
                .iter()
                .take(cap)
                .all(|candidate| candidate.id != keep_candidate.id)
        });

    let Some(keep_candidate) = keep_candidate else {
        return candidates.iter().take(cap).map(to_match).collect();
    };

    // Room for the kept entry comes from the tail; it is then reinserted in sorted order.
    let mut matches: Vec<StringMatch> = candidates
        .iter()
        .take(cap.saturating_sub(1))
        .map(to_match)
        .collect();
    let insert_at =
        matches.partition_point(|entry_match| entry_match.candidate_id < keep_candidate.id);
    matches.insert(insert_at, to_match(keep_candidate));
    matches
}

pub fn init(_cx: &mut App) {
    BREADCRUMB_PICKER_RENDERERS
        .set(BreadcrumbPickerRenderers {
            directory: directory::render_breadcrumb_directory_segment,
            symbol: symbol::render_breadcrumb_symbol_segment,
            popover_handle: default_popover_handle,
            symbol_popover_handle: default_symbol_popover_handle,
            segment_padding: SEGMENT_TRIGGER_PADDING,
        })
        .ok();
}

fn default_popover_handle() -> Rc<dyn ErasedBreadcrumbPopoverHandle> {
    Rc::new(BreadcrumbPopoverHandle::<
        directory::BreadcrumbDirectoryDelegate,
    >(Default::default()))
}

fn default_symbol_popover_handle() -> Rc<dyn ErasedBreadcrumbPopoverHandle> {
    Rc::new(BreadcrumbPopoverHandle::<symbol::BreadcrumbSymbolDelegate>(
        Default::default(),
    ))
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use editor::test::build_editor;
    use editor::{Editor, MultiBuffer};
    use gpui::{
        App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
        TestAppContext, VisualTestContext, Window,
    };
    use project::{FakeFs, Project, WorktreeId};
    use settings::KeymapFile;
    use util::path;
    use workspace::Workspace;

    pub(crate) fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let _app_state = workspace::AppState::test(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
    }

    /// `PopoverMenu`-free pickers need a real `Render` root to drive keystrokes through.
    pub(crate) struct Harness<P: Render> {
        pub(crate) picker: Option<Entity<P>>,
        pub(crate) editor: Entity<Editor>,
    }

    impl<P: Render> Render for Harness<P> {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            gpui::div().children(self.picker.clone())
        }
    }

    pub(crate) struct WorkspaceFixture {
        pub(crate) fs: Arc<FakeFs>,
        pub(crate) project: Entity<Project>,
        pub(crate) workspace: Entity<Workspace>,
        pub(crate) worktree_id: WorktreeId,
    }

    pub(crate) async fn workspace_fixture(
        tree: serde_json::Value,
        cx: &mut TestAppContext,
    ) -> WorkspaceFixture {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/root"), tree).await;
        let project = Project::test(fs.clone(), [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });
        let workspace_window =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let workspace = workspace_window.root(cx).unwrap();
        cx.run_until_parked();
        WorkspaceFixture {
            fs,
            project,
            workspace,
            worktree_id,
        }
    }

    pub(crate) struct PickerHarness<P: Render> {
        pub(crate) picker: Entity<P>,
        pub(crate) editor: Entity<Editor>,
        pub(crate) cx: VisualTestContext,
    }

    /// A `Harness` window whose picker attaches later: lets editor state the picker reads
    /// (e.g. the breadcrumb outline) resolve first.
    pub(crate) struct EditorFixture<P: Render> {
        harness: Entity<Harness<P>>,
        pub(crate) editor: Entity<Editor>,
        pub(crate) cx: VisualTestContext,
    }

    pub(crate) fn editor_fixture<P: Render>(
        buffer_text: &str,
        language: Option<Arc<language::Language>>,
        cx: &mut TestAppContext,
    ) -> EditorFixture<P> {
        let buffer = cx.new(|cx| {
            let mut buffer = language::Buffer::local(buffer_text, cx);
            if language.is_some() {
                buffer.set_language(language, cx);
            }
            buffer
        });
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let harness_window = cx.add_window(|window, cx| {
            let editor = cx.new(|cx| build_editor(buffer, window, cx));
            Harness {
                picker: None,
                editor,
            }
        });
        let harness = harness_window.root(cx).unwrap();
        let editor = cx.update(|cx| harness.read(cx).editor.clone());
        let cx = VisualTestContext::from_window(*harness_window, cx);
        EditorFixture {
            harness,
            editor,
            cx,
        }
    }

    impl<P: Render> EditorFixture<P> {
        pub(crate) fn attach_picker(
            mut self,
            build_picker: impl FnOnce(&Entity<Editor>, &mut Window, &mut App) -> Entity<P>,
        ) -> PickerHarness<P> {
            let picker = self.cx.update(|window, cx| {
                let picker = build_picker(&self.editor, window, cx);
                self.harness.update(cx, |harness, cx| {
                    harness.picker = Some(picker.clone());
                    cx.notify();
                });
                picker
            });
            PickerHarness {
                picker,
                editor: self.editor,
                cx: self.cx,
            }
        }
    }

    /// Builds a `Harness`-rooted window around a fresh editor and the delegate-specific picker
    /// `build_picker` constructs. No `run_until_parked`: some tests assert on in-flight state.
    pub(crate) fn picker_harness<P: Render>(
        buffer_text: &str,
        cx: &mut TestAppContext,
        build_picker: impl FnOnce(&Entity<Editor>, &mut Window, &mut App) -> Entity<P>,
    ) -> PickerHarness<P> {
        editor_fixture(buffer_text, None, cx).attach_picker(build_picker)
    }

    /// Binds the shipped context strings, minus the sibling `menu` block: shadowing there is uncaught.
    pub(crate) fn bind_drill_navigation_keymap(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.bind_keys(KeymapFile::load_panic_on_failure(
                r#"[
                    {
                        "context": "Editor",
                        "bindings": {
                            "left": "editor::MoveLeft",
                            "right": "editor::MoveRight"
                        }
                    },
                    {
                        "context": "Picker > Editor",
                        "bindings": {
                            "down": "menu::SelectNext",
                            "enter": "menu::Confirm"
                        }
                    },
                    {
                        "context": "BreadcrumbPicker > Editor",
                        "bindings": {
                            "left": "menu::SelectParent",
                            "right": "menu::SelectChild"
                        }
                    }
                ]"#,
                cx,
            ));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trigger grown past `SEGMENT_TRIGGER_PADDING` paints the trail wider than the row asked for.
    #[gpui::test]
    fn test_segment_trigger_padding_matches_the_registered_width(cx: &mut gpui::TestAppContext) {
        struct TriggerProbe {
            wrapped: bool,
            scroll_handle: gpui::ScrollHandle,
        }

        impl Render for TriggerProbe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                // Unshrinkable like the trigger, so only its padding shows up in the difference.
                let label = gpui::div()
                    .w(px(120.))
                    .h(px(10.))
                    .flex_none()
                    .into_any_element();
                let content = if self.wrapped {
                    crate::segment_trigger("probe", 0, label).into_any_element()
                } else {
                    label
                };
                h_flex()
                    .id("probe")
                    .w(px(40.))
                    .track_scroll(&self.scroll_handle)
                    .overflow_x_scroll()
                    .child(content)
            }
        }

        fn content_width(wrapped: bool, cx: &mut gpui::TestAppContext) -> Pixels {
            crate::test_support::init_test(cx);
            let scroll_handle = gpui::ScrollHandle::new();
            let window = cx.add_window({
                let scroll_handle = scroll_handle.clone();
                move |_, _| TriggerProbe {
                    wrapped,
                    scroll_handle,
                }
            });
            cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
                .unwrap();
            scroll_handle.max_offset().x + px(40.)
        }

        assert_eq!(
            content_width(true, cx) - content_width(false, cx),
            SEGMENT_TRIGGER_PADDING
        );
    }

    fn candidates(count: usize) -> Vec<StringMatchCandidate> {
        (0..count)
            .map(|index| StringMatchCandidate::new(index, &format!("entry-{index}")))
            .collect()
    }

    #[test]
    fn test_cap_empty_query_matches_passes_through_under_the_cap() {
        let candidates = candidates(5);
        let matches = cap_empty_query_matches(&candidates, None, 10);
        assert_eq!(matches.len(), 5);
    }

    #[test]
    fn test_cap_empty_query_matches_caps_and_keeps_an_entry_past_the_cap() {
        let candidates = candidates(10);
        let matches = cap_empty_query_matches(&candidates, Some(9), 3);

        assert_eq!(
            matches
                .iter()
                .map(|entry_match| entry_match.candidate_id)
                .collect::<Vec<_>>(),
            vec![0, 1, 9],
        );
    }

    #[test]
    fn test_cap_empty_query_matches_without_a_kept_id_just_truncates() {
        let candidates = candidates(10);
        let matches = cap_empty_query_matches(&candidates, None, 3);

        assert_eq!(
            matches
                .iter()
                .map(|entry_match| entry_match.candidate_id)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
        );
    }

    #[test]
    fn test_cap_empty_query_matches_drops_only_the_boundary_entry() {
        let candidates = candidates(300);
        let matches = cap_empty_query_matches(&candidates, Some(250), 200);

        let mut expected: Vec<usize> = (0..199).collect();
        expected.push(250);
        assert_eq!(
            matches
                .iter()
                .map(|entry_match| entry_match.candidate_id)
                .collect::<Vec<_>>(),
            expected,
        );
    }
}
