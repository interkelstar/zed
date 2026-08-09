use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use editor::{Anchor, Editor, ErasedBreadcrumbPopoverHandle, flatten_text_for_single_line_display};
use fuzzy::{StringMatch, StringMatchCandidate};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, ParentElement, Styled, StyledText, Task,
    WeakEntity, Window, div, rems,
};
use language::OutlineItem;
use picker::{Picker, PickerDelegate, PickerEditorPosition};
use project::WorktreeId;
use text::BufferId;
use ui::{
    ButtonLike, ButtonStyle, Color, Icon, IconName, IconSize, ListItem, ListItemSpacing,
    PopoverMenu, PopoverMenuHandle, prelude::*,
};
use util::rel_path::RelPath;
use workspace::ItemHandle;

use crate::MAX_BREADCRUMB_MENU_ENTRIES;

/// Newtype avoiding an orphan-rule conflict for `ErasedBreadcrumbPopoverHandle`.
pub(crate) struct SymbolPopoverHandle(pub PopoverMenuHandle<BreadcrumbSymbolPicker>);

impl ErasedBreadcrumbPopoverHandle for SymbolPopoverHandle {
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

pub struct BreadcrumbSymbolDelegate {
    editor: WeakEntity<Editor>,
    buffer_id: BufferId,
    /// The segment this popover is anchored under; `None` is the file segment.
    target: Option<OutlineItem<Anchor>>,
    items: Vec<OutlineItem<Anchor>>,
    /// Built once from `items` at construction; `update_matches` just clones the `Rc`.
    candidates: Rc<[StringMatchCandidate]>,
    matches: Vec<StringMatch>,
    query: String,
    selected_index: usize,
    current_range: Option<Range<Anchor>>,
    parent_dir: Option<(WorktreeId, Arc<RelPath>)>,
}

pub type BreadcrumbSymbolPicker = Picker<BreadcrumbSymbolDelegate>;

impl BreadcrumbSymbolDelegate {
    fn picker(
        editor: WeakEntity<Editor>,
        buffer_id: BufferId,
        target: Option<OutlineItem<Anchor>>,
        items: Vec<OutlineItem<Anchor>>,
        current_range: Option<Range<Anchor>>,
        parent_dir: Option<(WorktreeId, Arc<RelPath>)>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<BreadcrumbSymbolPicker> {
        cx.new(|cx| {
            let selected_index = current_range
                .as_ref()
                .and_then(|range| items.iter().position(|item| &item.range == range))
                .unwrap_or(0);
            let candidates = items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    StringMatchCandidate::new(
                        index,
                        &flatten_text_for_single_line_display(&item.text),
                    )
                })
                .collect::<Vec<_>>()
                .into();
            let delegate = Self {
                editor,
                buffer_id,
                target,
                items,
                candidates,
                matches: Vec::new(),
                query: String::new(),
                selected_index,
                current_range,
                parent_dir,
            };
            Picker::uniform_list(delegate, window, cx)
                .popover()
                .show_scrollbar(true)
                .initial_width(rems(18.))
        })
    }

    fn shows_current_marker(&self) -> bool {
        self.current_range
            .as_ref()
            .is_some_and(|range| self.items.iter().any(|item| &item.range == range))
    }

    fn item_at(&self, index: usize) -> Option<&OutlineItem<Anchor>> {
        self.items.get(self.matches.get(index)?.candidate_id)
    }
}

impl PickerDelegate for BreadcrumbSymbolDelegate {
    type ListItem = AnyElement;

    fn name() -> &'static str {
        "breadcrumb symbol picker"
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
        cx: &mut Context<BreadcrumbSymbolPicker>,
    ) {
        self.selected_index = index;
        cx.notify();
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Search symbols…".into()
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
        cx: &mut Context<BreadcrumbSymbolPicker>,
    ) -> Task<()> {
        self.query = query.clone();

        if query.is_empty() {
            self.matches = self
                .candidates
                .iter()
                .map(|candidate| StringMatch {
                    candidate_id: candidate.id,
                    string: candidate.string.clone(),
                    positions: Vec::new(),
                    score: 0.,
                })
                .collect();
            self.selected_index = self
                .current_range
                .as_ref()
                .and_then(|range| self.items.iter().position(|item| &item.range == range))
                .unwrap_or(0);
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
        cx: &mut Context<BreadcrumbSymbolPicker>,
    ) {
        let Some(item) = self.item_at(self.selected_index).cloned() else {
            return;
        };
        if let Some(editor) = self.editor.upgrade() {
            editor.update(cx, |editor, cx| {
                editor.navigate_to_outline_item(&item, window, cx);
            });
        }
        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _window: &mut Window, _cx: &mut Context<BreadcrumbSymbolPicker>) {}

    fn select_child(
        &mut self,
        window: &mut Window,
        cx: &mut Context<BreadcrumbSymbolPicker>,
    ) -> Option<String> {
        if !self.query.is_empty() {
            return None;
        }
        let selected = self.item_at(self.selected_index)?.clone();
        let editor = self.editor.upgrade()?;
        let children =
            editor
                .read(cx)
                .breadcrumb_symbol_menu_items(self.buffer_id, Some(&selected), cx);
        // The sibling fallback lists the item itself: a leaf, nothing to drill into.
        if children.is_empty() || children.iter().any(|item| item.range == selected.range) {
            return None;
        }
        editor.update(cx, |editor, cx| {
            editor.navigate_breadcrumb_symbol_to(self.buffer_id, Some(selected), window, cx);
        });
        Some(String::new())
    }

    fn select_parent(
        &mut self,
        window: &mut Window,
        cx: &mut Context<BreadcrumbSymbolPicker>,
    ) -> Option<String> {
        if !self.query.is_empty() {
            return None;
        }
        let editor = self.editor.upgrade()?;
        if let Some(target) = self.target.clone() {
            let parent = editor
                .read(cx)
                .breadcrumb_symbol_parent(self.buffer_id, &target, cx);
            editor.update(cx, |editor, cx| {
                editor.navigate_breadcrumb_symbol_to(self.buffer_id, parent, window, cx);
            });
            return Some(String::new());
        }
        let (worktree_id, parent_path) = self.parent_dir.clone()?;
        editor.update(cx, |editor, cx| {
            editor.navigate_breadcrumb_to(worktree_id, parent_path, window, cx);
        });
        Some(String::new())
    }

    /// Uses the symbol's own highlighting; fuzzy match positions are dropped to avoid a clash.
    fn render_match(
        &self,
        index: usize,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<BreadcrumbSymbolPicker>,
    ) -> Option<Self::ListItem> {
        let item = self.item_at(index)?;
        let is_current = self.current_range.as_ref() == Some(&item.range);

        let mut text_style = window.text_style();
        text_style.color = Color::Default.color(cx);

        Some(
            ListItem::new(SharedString::from(format!(
                "breadcrumb-symbol-entry-{index}"
            )))
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .when(self.shows_current_marker(), |this| {
                this.start_slot(div().flex_none().size(IconSize::Small.rems()).when(
                    is_current,
                    |this| {
                        this.child(
                            Icon::new(IconName::Check)
                                .color(Color::Accent)
                                .size(IconSize::Small),
                        )
                    },
                ))
            })
            .child(
                div().text_ui(cx).child(
                    StyledText::new(flatten_text_for_single_line_display(&item.text))
                        .with_default_highlights(&text_style, item.highlight_ranges.clone()),
                ),
            )
            .into_any_element(),
        )
    }
}

/// Decided once so the same click never flips between an empty popover and the outline modal.
enum BreadcrumbSymbolMenuOutcome {
    ShowPicker(Vec<OutlineItem<Anchor>>),
    ShowOutlineModal,
    OutlineNotReady,
}

fn breadcrumb_symbol_menu_outcome(
    editor: &Editor,
    buffer_id: BufferId,
    target: Option<&OutlineItem<Anchor>>,
    cx: &App,
) -> BreadcrumbSymbolMenuOutcome {
    let menu_items = editor.breadcrumb_symbol_menu_items(buffer_id, target, cx);
    if !menu_items.is_empty() {
        return BreadcrumbSymbolMenuOutcome::ShowPicker(menu_items);
    }
    if editor.breadcrumb_outline_ready(buffer_id, cx) {
        BreadcrumbSymbolMenuOutcome::ShowOutlineModal
    } else {
        BreadcrumbSymbolMenuOutcome::OutlineNotReady
    }
}

pub(crate) fn render_breadcrumb_symbol_segment(
    editor: WeakEntity<Editor>,
    buffer_id: BufferId,
    target: Option<OutlineItem<Anchor>>,
    parent_dir: Option<(WorktreeId, Arc<RelPath>)>,
    is_active_segment: bool,
    shared_popover_handle: Rc<dyn ErasedBreadcrumbPopoverHandle>,
    label: gpui::AnyElement,
    index: usize,
) -> gpui::AnyElement {
    // `ButtonLike` stops propagation, keeping this click off the outline toggle behind it.
    let trigger = ButtonLike::new(("breadcrumb-symbol", index))
        .style(ButtonStyle::Transparent)
        .size(ui::ButtonSize::None)
        .height(rems_from_px(22.).into())
        .child(label);

    // Only the active segment carries the handle `Editor::navigate_breadcrumb_symbol_to` reopens through.
    let popover_handle = if is_active_segment {
        shared_popover_handle
            .as_any()
            .downcast_ref::<SymbolPopoverHandle>()
            .map(|handle| handle.0.clone())
            .unwrap_or_default()
    } else {
        PopoverMenuHandle::default()
    };

    let menu = PopoverMenu::new(("breadcrumb-symbol-menu", index)).with_handle(popover_handle);
    let menu = if target.is_none() {
        menu.trigger_with_tooltip(trigger, ui::Tooltip::text("Right-Click to Copy Path"))
    } else {
        menu.trigger(trigger)
    };
    menu.menu(move |window, cx| {
        let editor_entity = editor.upgrade()?;
        let menu_items = match breadcrumb_symbol_menu_outcome(
            editor_entity.read(cx),
            buffer_id,
            target.as_ref(),
            cx,
        ) {
            BreadcrumbSymbolMenuOutcome::ShowPicker(menu_items) => menu_items,
            BreadcrumbSymbolMenuOutcome::OutlineNotReady => {
                // A silent click is worse than the modal: fetch, and still do something now.
                editor_entity.update(cx, |editor, cx| {
                    editor.prefetch_breadcrumb_outline(buffer_id, cx);
                });
                if let Some(callback) = zed_actions::outline::TOGGLE_OUTLINE.get() {
                    callback(editor_entity.to_any_view(), window, cx);
                }
                return None;
            }
            BreadcrumbSymbolMenuOutcome::ShowOutlineModal => {
                if let Some(callback) = zed_actions::outline::TOGGLE_OUTLINE.get() {
                    callback(editor_entity.to_any_view(), window, cx);
                }
                return None;
            }
        };

        editor_entity.update(cx, |editor, cx| {
            editor.open_breadcrumb_symbol_navigation(buffer_id, target.clone(), cx);
        });

        let picker = BreadcrumbSymbolDelegate::picker(
            editor.clone(),
            buffer_id,
            target.clone(),
            menu_items,
            target.as_ref().map(|item| item.range.clone()),
            parent_dir.clone(),
            window,
            cx,
        );
        editor_entity.update(cx, |editor, cx| {
            editor.watch_breadcrumb_symbol_dismissal(&picker, buffer_id, target.clone(), cx);
        });
        Some(picker)
    })
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    use editor::MultiBuffer;
    use editor::MultiBufferSnapshot;
    use editor::test::build_editor;
    use gpui::{Focusable, TestAppContext, VisualTestContext};
    use std::cell::Cell;
    use text::Point;

    use crate::test_support::{Harness, bind_drill_navigation_keymap};

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let _app_state = workspace::AppState::test(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            crate::init(cx);
        });
    }

    fn test_item(snapshot: &MultiBufferSnapshot, row: u32, text: &str) -> OutlineItem<Anchor> {
        let range =
            snapshot.anchor_before(Point::new(row, 0))..snapshot.anchor_after(Point::new(row, 0));
        OutlineItem {
            depth: 0,
            range: range.clone(),
            selection_range: range.clone(),
            source_range_for_text: range,
            text: text.into(),
            highlight_ranges: Vec::new(),
            name_ranges: Vec::new(),
            body_range: None,
            annotation_range: None,
        }
    }

    fn build_test_editor(cx: &mut TestAppContext) -> (gpui::WindowHandle<Editor>, Entity<Editor>) {
        let buffer = cx.new(|cx| language::Buffer::local("alpha\nbeta\ngamma\n", cx));
        let multi_buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let editor_window =
            cx.add_window(|window, cx| build_editor(multi_buffer.clone(), window, cx));
        let editor = editor_window.root(cx).unwrap();
        (editor_window, editor)
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_picker_preselects_current_symbol(cx: &mut TestAppContext) {
        init_test(cx);

        let (editor_window, editor) = build_test_editor(cx);
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let snapshot = editor.read_with(cx, |editor, cx| editor.buffer().read(cx).snapshot(cx));
        let items = vec![
            test_item(&snapshot, 0, "alpha"),
            test_item(&snapshot, 1, "beta"),
            test_item(&snapshot, 2, "gamma"),
        ];
        let current_range = items[1].range.clone();

        let picker = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbSymbolDelegate::picker(
                    editor.downgrade(),
                    BufferId::new(1).unwrap(),
                    None,
                    items,
                    Some(current_range),
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();

        picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker.delegate.selected_index, 1,
                "the segment's own symbol is preselected"
            );
        });

        picker
            .update_in(cx, |picker, window, cx| {
                picker.delegate.update_matches(String::new(), window, cx)
            })
            .await;

        picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker.delegate.selected_index, 1,
                "clearing the query keeps the current symbol selected"
            );
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_picker_fuzzy_filtering_resets_selection(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let (editor_window, editor) = build_test_editor(cx);
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let snapshot = editor.read_with(cx, |editor, cx| editor.buffer().read(cx).snapshot(cx));
        let items = vec![
            test_item(&snapshot, 0, "alpha"),
            test_item(&snapshot, 1, "beta"),
            test_item(&snapshot, 2, "gamma"),
        ];

        let picker = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbSymbolDelegate::picker(
                    editor.downgrade(),
                    BufferId::new(1).unwrap(),
                    None,
                    items,
                    None,
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();

        picker
            .update_in(cx, |picker, window, cx| {
                picker
                    .delegate
                    .update_matches("gam".to_string(), window, cx)
            })
            .await;

        picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker.delegate.matches.len(),
                1,
                "the query narrows the listing to the one discriminating match"
            );
            assert_eq!(
                picker.delegate.item_at(0).map(|item| item.text.as_ref()),
                Some("gamma")
            );
            assert_eq!(
                picker.delegate.selected_index, 0,
                "filtering resets the selection to the top match"
            );
        });
    }

    #[gpui::test]
    async fn test_update_matches_reuses_cached_candidates(cx: &mut TestAppContext) {
        init_test(cx);

        let (editor_window, editor) = build_test_editor(cx);
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let snapshot = editor.read_with(cx, |editor, cx| editor.buffer().read(cx).snapshot(cx));
        let items = vec![
            test_item(&snapshot, 0, "alpha"),
            test_item(&snapshot, 1, "beta"),
            test_item(&snapshot, 2, "gamma"),
        ];

        let picker = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbSymbolDelegate::picker(
                    editor.downgrade(),
                    BufferId::new(1).unwrap(),
                    None,
                    items,
                    None,
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();

        let candidates_before =
            picker.read_with(cx, |picker, _| picker.delegate.candidates.clone());

        picker
            .update_in(cx, |picker, window, cx| {
                picker
                    .delegate
                    .update_matches("gam".to_string(), window, cx)
            })
            .await;
        picker
            .update_in(cx, |picker, window, cx| {
                picker.delegate.update_matches("ga".to_string(), window, cx)
            })
            .await;

        picker.read_with(cx, |picker, _| {
            assert!(
                Rc::ptr_eq(&candidates_before, &picker.delegate.candidates),
                "successive keystrokes must reuse the same candidate list, not rebuild it"
            );
        });
    }

    #[gpui::test]
    async fn test_confirming_breadcrumb_symbol_navigates_and_dismisses(cx: &mut TestAppContext) {
        init_test(cx);

        let (editor_window, editor) = build_test_editor(cx);
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);
        let buffer_id = BufferId::new(1).unwrap();

        let snapshot = editor.read_with(cx, |editor, cx| editor.buffer().read(cx).snapshot(cx));
        let items = vec![
            test_item(&snapshot, 0, "alpha"),
            test_item(&snapshot, 1, "beta"),
            test_item(&snapshot, 2, "gamma"),
        ];

        editor.update(cx, |editor, cx| {
            editor.open_breadcrumb_symbol_navigation(buffer_id, None, cx);
        });

        let picker = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbSymbolDelegate::picker(
                    editor.downgrade(),
                    buffer_id,
                    None,
                    items,
                    None,
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();
        editor.update(cx, |editor, cx| {
            editor.watch_breadcrumb_symbol_dismissal(&picker, buffer_id, None, cx);
        });

        let dismissed = Rc::new(Cell::new(false));
        let subscription = cx.update(|_, cx| {
            cx.subscribe(&picker, {
                let dismissed = dismissed.clone();
                move |_, _: &DismissEvent, _| dismissed.set(true)
            })
        });

        picker.update_in(cx, |picker, window, cx| {
            picker.delegate.selected_index = 2;
            picker.delegate.confirm(false, window, cx);
        });
        cx.run_until_parked();

        editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let cursor = editor.selections.newest::<Point>(&snapshot).head();
            assert_eq!(
                cursor,
                Point::new(2, 0),
                "confirming navigates the cursor to the chosen symbol"
            );
        });
        assert!(dismissed.get(), "confirming a row dismisses the popover");
        editor.read_with(cx, |editor, _| {
            assert!(
                editor.breadcrumb_symbol_navigation().is_none(),
                "the dismissal must clear the navigation session confirm just ended"
            );
        });
        drop(subscription);
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_picker_navigates_from_the_keyboard(cx: &mut TestAppContext) {
        init_test(cx);

        let buffer = cx.new(|cx| language::Buffer::local("alpha\nbeta\ngamma\n", cx));
        let multi_buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));

        let harness_window = cx.add_window(|window, cx| {
            let editor = cx.new(|cx| build_editor(multi_buffer.clone(), window, cx));
            let snapshot = editor.read(cx).buffer().read(cx).snapshot(cx);
            let items = vec![
                test_item(&snapshot, 0, "alpha"),
                test_item(&snapshot, 1, "beta"),
                test_item(&snapshot, 2, "gamma"),
            ];
            let picker = BreadcrumbSymbolDelegate::picker(
                editor.downgrade(),
                BufferId::new(1).unwrap(),
                None,
                items,
                None,
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
                    .item_at(picker.delegate.selected_index)
                    .map(|item| item.text.as_ref()),
                Some("beta"),
            );
        });

        cx.dispatch_action(menu::Confirm);
        cx.run_until_parked();

        editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let cursor = editor.selections.newest::<Point>(&snapshot).head();
            assert_eq!(
                cursor,
                Point::new(1, 0),
                "confirming the selected row navigates the cursor there"
            );
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_select_parent_with_a_target_navigates_the_editor(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let (editor_window, editor) = build_test_editor(cx);
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let snapshot = editor.read_with(cx, |editor, cx| editor.buffer().read(cx).snapshot(cx));
        let target = test_item(&snapshot, 0, "alpha");
        let buffer_id = BufferId::new(1).unwrap();

        let picker = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbSymbolDelegate::picker(
                    editor.downgrade(),
                    buffer_id,
                    Some(target),
                    vec![test_item(&snapshot, 0, "alpha")],
                    None,
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();

        picker.update_in(cx, |picker, window, cx| {
            picker.delegate.select_parent(window, cx);
        });
        cx.run_until_parked();

        editor.read_with(cx, |editor, _| {
            assert!(
                editor.breadcrumb_symbol_navigation().is_some(),
                "select_parent with a target segment navigates the editor's symbol state"
            );
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_select_parent_without_a_target_or_parent_dir_is_a_noop(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let (editor_window, editor) = build_test_editor(cx);
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);

        let picker = editor_window
            .update(cx, |_, window, cx| {
                BreadcrumbSymbolDelegate::picker(
                    editor.downgrade(),
                    BufferId::new(1).unwrap(),
                    None,
                    Vec::new(),
                    None,
                    None,
                    window,
                    cx,
                )
            })
            .unwrap();

        let result = picker.update_in(cx, |picker, window, cx| {
            picker.delegate.select_parent(window, cx)
        });
        assert!(
            result.is_none(),
            "the file segment with no parent directory has nowhere to go"
        );
        editor.read_with(cx, |editor, _| {
            assert!(editor.breadcrumb_symbol_navigation().is_none());
        });
    }

    #[gpui::test]
    async fn test_select_parent_and_child_do_not_drill_with_a_query(cx: &mut TestAppContext) {
        init_test(cx);
        bind_drill_navigation_keymap(cx);

        let buffer = cx.new(|cx| language::Buffer::local("alpha\nbeta\ngamma\n", cx));
        let multi_buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        let buffer_id = BufferId::new(1).unwrap();

        let harness_window = cx.add_window(|window, cx| {
            let editor = cx.new(|cx| build_editor(multi_buffer.clone(), window, cx));
            let snapshot = editor.read(cx).buffer().read(cx).snapshot(cx);
            let target = test_item(&snapshot, 0, "alpha");
            let picker = BreadcrumbSymbolDelegate::picker(
                editor.downgrade(),
                buffer_id,
                Some(target),
                vec![test_item(&snapshot, 0, "alpha")],
                None,
                None,
                window,
                cx,
            );
            Harness { picker, editor }
        });
        let (picker, _editor) = harness_window
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
        cx.simulate_keystrokes("a l p h a");
        cx.run_until_parked();
        picker.update(cx, |picker, cx| {
            assert_eq!(picker.query(cx), "alpha");
        });

        // A non-empty query leaves left/right for the caret, even with a target set: a swallowed
        // key would leave the typed letter appended at the end instead of landing at the caret.
        cx.simulate_keystrokes("left");
        cx.simulate_keystrokes("z");
        cx.run_until_parked();
        picker.update(cx, |picker, cx| {
            assert_eq!(picker.query(cx), "alphza");
        });

        cx.simulate_keystrokes("right");
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        picker.update(cx, |picker, cx| {
            assert_eq!(picker.query(cx), "alphzay");
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_menu_outcome_is_not_ready_before_prefetch(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let (editor_window, editor) = build_test_editor(cx);
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);
        let buffer_id = editor.read_with(cx, |editor, cx| {
            editor
                .buffer()
                .read(cx)
                .as_singleton()
                .unwrap()
                .read(cx)
                .remote_id()
        });

        editor.read_with(cx, |editor, cx| {
            let outcome = breadcrumb_symbol_menu_outcome(editor, buffer_id, None, cx);
            assert!(
                matches!(outcome, BreadcrumbSymbolMenuOutcome::OutlineNotReady),
                "the outline has not been fetched yet, so the picker's items are not known; \
                 the caller decides whether that means a prefetch-and-fall-back-to-modal"
            );
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_menu_outcome_falls_back_to_modal_when_genuinely_empty(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let (editor_window, editor) = build_test_editor(cx);
        let cx = &mut VisualTestContext::from_window(*editor_window, cx);
        let buffer_id = editor.read_with(cx, |editor, cx| {
            editor
                .buffer()
                .read(cx)
                .as_singleton()
                .unwrap()
                .read(cx)
                .remote_id()
        });

        // A resolved language with no outline query still answers, just with nothing.
        editor.update(cx, |editor, cx| {
            let buffer = editor.buffer().read(cx).as_singleton().unwrap().clone();
            buffer.update(cx, |buffer, cx| {
                buffer.set_language(Some(language::PLAIN_TEXT.clone()), cx)
            });
        });
        cx.run_until_parked();

        editor.update(cx, |editor, cx| {
            editor.prefetch_breadcrumb_outline(buffer_id, cx);
        });
        cx.run_until_parked();

        editor.read_with(cx, |editor, cx| {
            let outcome = breadcrumb_symbol_menu_outcome(editor, buffer_id, None, cx);
            assert!(
                matches!(outcome, BreadcrumbSymbolMenuOutcome::ShowOutlineModal),
                "a loaded outline with no items must fall back to the outline modal"
            );
        });
    }
}
