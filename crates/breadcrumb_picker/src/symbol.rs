use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use editor::{Anchor, Editor, ErasedBreadcrumbPopoverHandle, flatten_text_for_single_line_display};
use fuzzy::{StringMatch, StringMatchCandidate};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, FontWeight, HighlightStyle, ParentElement,
    Styled, StyledText, Task, WeakEntity, Window, div, rems,
};
use language::OutlineItem;
use picker::{Picker, PickerDelegate, PickerEditorPosition};
use project::WorktreeId;
use text::BufferId;
use ui::{Color, Icon, IconName, IconSize, ListItem, ListItemSpacing, PopoverMenu, prelude::*};
use util::rel_path::RelPath;

use crate::{BreadcrumbPickerCore, BreadcrumbPickerDelegate};

pub struct BreadcrumbSymbolDelegate {
    editor: WeakEntity<Editor>,
    buffer_id: BufferId,
    /// The segment this popover is anchored under; `None` is the file segment.
    target: Option<OutlineItem<Anchor>>,
    items: Vec<OutlineItem<Anchor>>,
    core: BreadcrumbPickerCore,
    pending_selection_key: Option<Range<Anchor>>,
    parent_dir: Option<(WorktreeId, Arc<RelPath>)>,
    /// The outline had not answered yet when the popover opened; cleared once `watch_outline_ready`'s task lands.
    loading: bool,
    _loading_task: Task<()>,
}

pub type BreadcrumbSymbolPicker = Picker<BreadcrumbSymbolDelegate>;

impl BreadcrumbSymbolDelegate {
    fn picker(
        editor: WeakEntity<Editor>,
        buffer_id: BufferId,
        target: Option<OutlineItem<Anchor>>,
        items: Vec<OutlineItem<Anchor>>,
        loading: bool,
        parent_dir: Option<(WorktreeId, Arc<RelPath>)>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<BreadcrumbSymbolPicker> {
        cx.new(|cx| {
            let mut delegate = Self {
                editor: editor.clone(),
                buffer_id,
                target: target.clone(),
                items: Vec::new(),
                core: BreadcrumbPickerCore::default(),
                pending_selection_key: None,
                parent_dir,
                loading,
                _loading_task: Task::ready(()),
            };
            delegate.reset_items(items);
            let mut picker = Picker::uniform_list(delegate, window, cx)
                .popover()
                .show_scrollbar(true)
                .initial_width(rems(18.));
            if loading {
                picker
                    .delegate
                    .watch_outline_ready(editor, buffer_id, target, window, cx);
            }
            picker
        })
    }

    /// Rebuilds `candidates` for new `items`, then rebuilds the empty-query display from them.
    fn reset_items(&mut self, items: Vec<OutlineItem<Anchor>>) {
        crate::record_pending_selection(self);
        self.core.candidates = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                StringMatchCandidate::new(index, &flatten_text_for_single_line_display(&item.text))
            })
            .collect::<Vec<_>>()
            .into();
        debug_assert!(
            self.core
                .candidates
                .iter()
                .enumerate()
                .all(|(index, candidate)| candidate.id == index)
        );
        self.items = items;
        // A non-empty query keeps whatever `matches` it already has; `update_matches`'s refresh re-runs the filter.
        if self.core.query.is_empty() {
            crate::apply_empty_query_matches(self);
        }
    }

    fn current_range(&self) -> Option<&Range<Anchor>> {
        self.target.as_ref().map(|item| &item.range)
    }

    /// Waits for the prefetch the caller already kicked off, then refreshes the listing.
    fn watch_outline_ready(
        &mut self,
        editor: WeakEntity<Editor>,
        buffer_id: BufferId,
        target: Option<OutlineItem<Anchor>>,
        window: &mut Window,
        cx: &mut Context<BreadcrumbSymbolPicker>,
    ) {
        let Ok(task) = editor.read_with(cx, |editor, _| editor.breadcrumb_outline_prefetch_task())
        else {
            return;
        };
        self._loading_task = cx.spawn_in(window, async move |picker, cx| {
            task.await;
            picker
                .update_in(cx, |picker, window, cx| {
                    let Some(items) = editor
                        .read_with(cx, |editor, cx| {
                            editor.breadcrumb_symbol_menu_items(buffer_id, target.as_ref(), cx)
                        })
                        .ok()
                    else {
                        return;
                    };
                    picker.delegate.loading = false;
                    picker.delegate.reset_items(items);
                    picker.refresh(window, cx);
                })
                .ok();
        });
    }

    fn shows_current_marker(&self) -> bool {
        (0..self.items.len()).any(|candidate_id| self.is_current_candidate(candidate_id))
    }

    fn item_at(&self, index: usize) -> Option<&OutlineItem<Anchor>> {
        self.items.get(self.core.matches.get(index)?.candidate_id)
    }
}

impl BreadcrumbPickerDelegate for BreadcrumbSymbolDelegate {
    type SelectionKey = Range<Anchor>;

    fn core(&self) -> &BreadcrumbPickerCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut BreadcrumbPickerCore {
        &mut self.core
    }

    fn is_current_candidate(&self, candidate_id: usize) -> bool {
        self.current_range().is_some_and(|range| {
            self.items
                .get(candidate_id)
                .is_some_and(|item| &item.range == range)
        })
    }

    fn selection_key(&self, candidate_id: usize) -> Option<Range<Anchor>> {
        self.items.get(candidate_id).map(|item| item.range.clone())
    }

    fn matches_selection_key(&self, candidate_id: usize, selection_key: &Range<Anchor>) -> bool {
        self.items
            .get(candidate_id)
            .is_some_and(|item| &item.range == selection_key)
    }

    fn pending_selection_key(&self) -> &Option<Range<Anchor>> {
        &self.pending_selection_key
    }

    fn pending_selection_key_mut(&mut self) -> &mut Option<Range<Anchor>> {
        &mut self.pending_selection_key
    }
}

impl PickerDelegate for BreadcrumbSymbolDelegate {
    type ListItem = AnyElement;

    fn name() -> &'static str {
        "breadcrumb symbol picker"
    }

    fn match_count(&self) -> usize {
        self.core.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.core.selected_index
    }

    fn set_selected_index(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<BreadcrumbSymbolPicker>,
    ) {
        self.core.selected_index = index;
        cx.notify();
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Search symbols…".into()
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        Some(if self.loading {
            "Loading symbols…".into()
        } else if !self.core.query.is_empty() {
            "No matches".into()
        } else {
            "No symbols".into()
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
        cx: &mut Context<BreadcrumbSymbolPicker>,
    ) -> Task<()> {
        crate::update_picker_matches(self, query, cx)
    }

    fn confirm(
        &mut self,
        _secondary: bool,
        window: &mut Window,
        cx: &mut Context<BreadcrumbSymbolPicker>,
    ) {
        let Some(item) = self.item_at(self.core.selected_index).cloned() else {
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
        if !self.core.query.is_empty() {
            return None;
        }
        let selected = self.item_at(self.core.selected_index)?.clone();
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
        if !self.core.query.is_empty() {
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

    fn render_match(
        &self,
        index: usize,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<BreadcrumbSymbolPicker>,
    ) -> Option<Self::ListItem> {
        let item = self.item_at(index)?;
        let string_match = self.core.matches.get(index)?;
        let is_current = self.current_range() == Some(&item.range);

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
            .child({
                let flattened_text = flatten_text_for_single_line_display(&item.text);
                let highlights = match_emphasized_highlights(item, &flattened_text, string_match);
                div().text_ui(cx).child(
                    StyledText::new(flattened_text)
                        .with_default_highlights(&text_style, highlights),
                )
            })
            .into_any_element(),
        )
    }
}

/// A weight-only overlay keeps the syntax highlight's color visible under the match emphasis.
fn match_emphasized_highlights(
    item: &OutlineItem<Anchor>,
    flattened_text: &str,
    string_match: &StringMatch,
) -> Vec<(Range<usize>, HighlightStyle)> {
    // A kept stale match can carry the item's old text; its ranges would trip StyledText's checks.
    if flattened_text != string_match.string {
        return item.highlight_ranges.iter().cloned().collect();
    }
    let bold = HighlightStyle {
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    gpui::combine_highlights(
        string_match.ranges().map(|range| (range, bold)),
        item.highlight_ranges.iter().cloned(),
    )
    .collect()
}

/// The popover always opens; this only decides what it shows, so a click never picks between an empty popover and the outline modal.
#[derive(Debug, PartialEq)]
enum BreadcrumbSymbolMenuOutcome {
    Ready(Vec<OutlineItem<Anchor>>),
    /// The outline answered, and it is genuinely empty.
    Empty,
    /// The outline has not answered yet; the caller kicks a prefetch alongside this.
    NotReady,
}

fn breadcrumb_symbol_menu_outcome(
    editor: &Editor,
    buffer_id: BufferId,
    target: Option<&OutlineItem<Anchor>>,
    cx: &App,
) -> BreadcrumbSymbolMenuOutcome {
    let menu_items = editor.breadcrumb_symbol_menu_items(buffer_id, target, cx);
    if !menu_items.is_empty() {
        return BreadcrumbSymbolMenuOutcome::Ready(menu_items);
    }
    if editor.breadcrumb_outline_ready(buffer_id, cx) {
        BreadcrumbSymbolMenuOutcome::Empty
    } else {
        BreadcrumbSymbolMenuOutcome::NotReady
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
    let trigger = crate::segment_trigger("breadcrumb-symbol", index, label);
    let popover_handle = crate::segment_popover_handle::<BreadcrumbSymbolDelegate>(
        is_active_segment,
        shared_popover_handle,
    );

    let menu = PopoverMenu::new(("breadcrumb-symbol-menu", index)).with_handle(popover_handle);
    let copy_title = if target.is_none() {
        "Copy File Path"
    } else {
        "Copy Path with Line Number"
    };
    let menu = menu.trigger_with_tooltip(trigger, move |_, cx| {
        ui::Tooltip::with_meta(copy_title, None, "Right click", cx)
    });
    menu.menu(move |window, cx| {
        let editor_entity = editor.upgrade()?;
        let (menu_items, loading) = match breadcrumb_symbol_menu_outcome(
            editor_entity.read(cx),
            buffer_id,
            target.as_ref(),
            cx,
        ) {
            BreadcrumbSymbolMenuOutcome::Ready(menu_items) => (menu_items, false),
            BreadcrumbSymbolMenuOutcome::Empty => (Vec::new(), false),
            BreadcrumbSymbolMenuOutcome::NotReady => {
                editor_entity.update(cx, |editor, cx| {
                    editor.prefetch_breadcrumb_outline(buffer_id, cx);
                });
                (Vec::new(), true)
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
            loading,
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

    use gpui::{Focusable, TestAppContext, VisualTestContext};
    use std::cell::Cell;
    use text::Point;

    use crate::test_support::{
        EditorFixture, bind_drill_navigation_keymap, editor_fixture, init_test,
    };

    const SOURCE: &str = "struct Alpha {\n    one: u32,\n}\n\nimpl Alpha {\n    fn beta(&self) {}\n\n    fn gamma(&self) {}\n}\n";

    struct SymbolTest {
        picker: Entity<BreadcrumbSymbolPicker>,
        editor: Entity<Editor>,
        buffer_id: BufferId,
        cx: VisualTestContext,
    }

    fn singleton_buffer_id(editor: &Entity<Editor>, cx: &mut VisualTestContext) -> BufferId {
        editor.read_with(cx, |editor, cx| {
            editor
                .buffer()
                .read(cx)
                .as_singleton()
                .unwrap()
                .read(cx)
                .remote_id()
        })
    }

    fn outline_item_named(
        editor: &Editor,
        buffer_id: BufferId,
        text: &str,
        cx: &App,
    ) -> OutlineItem<Anchor> {
        let mut queue = editor.breadcrumb_symbol_menu_items(buffer_id, None, cx);
        while let Some(item) = queue.pop() {
            if item.text.as_ref() == text {
                return item;
            }
            queue.extend(
                editor
                    .breadcrumb_symbol_menu_items(buffer_id, Some(&item), cx)
                    .into_iter()
                    .filter(|child| child.depth > item.depth),
            );
        }
        panic!("no outline item named {text:?} in the test source");
    }

    fn resolved_outline_fixture(cx: &mut TestAppContext) -> EditorFixture<BreadcrumbSymbolPicker> {
        let mut fixture = editor_fixture(SOURCE, Some(language::rust_lang()), cx);
        let editor = fixture.editor.clone();
        let buffer_id = singleton_buffer_id(&editor, &mut fixture.cx);
        fixture.cx.run_until_parked();
        editor.update_in(&mut fixture.cx, |editor, _, cx| {
            editor.prefetch_breadcrumb_outline(buffer_id, cx);
        });
        fixture.cx.run_until_parked();
        fixture
    }

    /// A picker over `SOURCE`'s real outline, fed exactly what the production popover gets:
    /// the editor's own buffer id and the resolved menu items for `target`.
    fn symbol_test(target_text: Option<&str>, cx: &mut TestAppContext) -> SymbolTest {
        let mut fixture = resolved_outline_fixture(cx);
        let editor = fixture.editor.clone();
        let buffer_id = singleton_buffer_id(&editor, &mut fixture.cx);
        let (target, items) = editor.read_with(&mut fixture.cx, |editor, cx| {
            let target = target_text.map(|text| outline_item_named(editor, buffer_id, text, cx));
            let items = editor.breadcrumb_symbol_menu_items(buffer_id, target.as_ref(), cx);
            (target, items)
        });
        assert!(!items.is_empty(), "the outline must have resolved");
        let harness = fixture.attach_picker(|editor, window, cx| {
            BreadcrumbSymbolDelegate::picker(
                editor.downgrade(),
                buffer_id,
                target,
                items,
                false,
                None,
                window,
                cx,
            )
        });
        SymbolTest {
            picker: harness.picker,
            editor: harness.editor,
            buffer_id,
            cx: harness.cx,
        }
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_picker_filtering_follows_the_query(cx: &mut TestAppContext) {
        init_test(cx);

        let mut t = symbol_test(Some("fn gamma"), cx);
        let cx = &mut t.cx;

        t.picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker.delegate.item_at(1).map(|item| item.text.as_ref()),
                Some("fn gamma"),
                "a leaf target lists its siblings"
            );
            assert_eq!(
                picker.delegate.core.selected_index, 1,
                "the segment's own symbol is preselected"
            );
        });

        let candidates_before = t
            .picker
            .read_with(cx, |picker, _| picker.delegate.core.candidates.clone());

        t.picker
            .update_in(cx, |picker, window, cx| {
                picker.delegate.update_matches(String::new(), window, cx)
            })
            .await;
        t.picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker.delegate.core.selected_index, 1,
                "clearing the query keeps the current symbol selected"
            );
        });

        t.picker
            .update_in(cx, |picker, window, cx| {
                picker
                    .delegate
                    .update_matches("gam".to_string(), window, cx)
            })
            .await;
        t.picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker.delegate.core.matches.len(),
                1,
                "the query narrows the listing to the one discriminating match"
            );
            assert_eq!(
                picker.delegate.item_at(0).map(|item| item.text.as_ref()),
                Some("fn gamma")
            );
            assert_eq!(
                picker.delegate.core.selected_index, 0,
                "filtering resets the selection to the top match"
            );
        });

        t.picker
            .update_in(cx, |picker, window, cx| {
                picker.delegate.update_matches("ga".to_string(), window, cx)
            })
            .await;
        t.picker.read_with(cx, |picker, _| {
            assert!(
                Rc::ptr_eq(&candidates_before, &picker.delegate.core.candidates),
                "successive keystrokes must reuse the same candidate list, not rebuild it"
            );
        });
    }

    #[gpui::test]
    async fn test_a_late_outline_keeps_the_selected_symbol(cx: &mut TestAppContext) {
        init_test(cx);

        let mut t = symbol_test(Some("fn gamma"), cx);
        let cx = &mut t.cx;

        t.picker.update_in(cx, |picker, window, cx| {
            window.focus(&picker.focus_handle(cx), cx);
            picker.set_query("a", window, cx);
        });
        cx.run_until_parked();

        let selected_range = t.picker.update_in(cx, |picker, window, cx| {
            let index = picker
                .delegate
                .core
                .matches
                .iter()
                .position(|entry_match| {
                    picker.delegate.items[entry_match.candidate_id]
                        .text
                        .as_ref()
                        == "fn gamma"
                })
                .expect("the query lists the sibling method");
            picker.delegate.set_selected_index(index, window, cx);
            picker.delegate.items[picker.delegate.core.matches[index].candidate_id]
                .range
                .clone()
        });

        // The arriving outline adds a symbol that sorts ahead of the selected one.
        let extra_item = t.editor.read_with(cx, |editor, cx| {
            outline_item_named(editor, t.buffer_id, "struct Alpha", cx)
        });
        let updated_items = t.picker.read_with(cx, |picker, _| {
            let mut items = vec![extra_item];
            items.extend(picker.delegate.items.iter().cloned());
            items
        });

        t.picker.update_in(cx, |picker, window, cx| {
            picker.delegate.reset_items(updated_items);
            picker.refresh(window, cx);
        });
        cx.run_until_parked();

        t.picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker.delegate.core.matches.len(),
                3,
                "the added symbol matches the query too"
            );
            assert_ne!(
                picker.delegate.item_at(0).map(|item| item.text.as_ref()),
                Some("fn gamma"),
                "the selected symbol must not also be the best match, or this test asserts nothing"
            );
            assert_eq!(
                picker
                    .delegate
                    .item_at(picker.delegate.core.selected_index)
                    .map(|item| item.range.clone()),
                Some(selected_range),
                "a late outline keeps the symbol the user selected"
            );
        });
    }

    #[gpui::test]
    async fn test_queried_rows_carry_syntax_highlights_and_bold_match_emphasis(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let syntax = HighlightStyle {
            color: Some(gpui::red()),
            ..Default::default()
        };
        let mut t = symbol_test(None, cx);
        let cx = &mut t.cx;
        t.picker.update(cx, |picker, _| {
            for item in &mut picker.delegate.items {
                item.highlight_ranges = vec![(0..item.text.len(), syntax)];
            }
        });

        t.picker
            .update_in(cx, |picker, window, cx| {
                picker
                    .delegate
                    .update_matches("struct".to_string(), window, cx)
            })
            .await;

        let (highlights, positions, text_len) = t.picker.read_with(cx, |picker, _| {
            let item = picker.delegate.item_at(0).expect("one row matches");
            assert_eq!(item.text.as_ref(), "struct Alpha");
            let string_match = picker
                .delegate
                .core
                .matches
                .first()
                .expect("one row matches");
            assert!(
                !string_match.positions.is_empty(),
                "a non-empty query must carry its match positions"
            );
            (
                match_emphasized_highlights(
                    item,
                    &flatten_text_for_single_line_display(&item.text),
                    string_match,
                ),
                string_match.positions.clone(),
                item.text.len(),
            )
        });

        let bold_at = |offset: usize| {
            highlights.iter().any(|(range, style)| {
                range.contains(&offset) && style.font_weight == Some(FontWeight::BOLD)
            })
        };
        let syntax_at = |offset: usize| {
            highlights
                .iter()
                .any(|(range, style)| range.contains(&offset) && style.color == Some(gpui::red()))
        };
        for &offset in &positions {
            assert!(bold_at(offset), "matched byte {offset} must render bold");
            assert!(
                syntax_at(offset),
                "matched byte {offset} must keep its syntax color"
            );
        }
        let unmatched = text_len - 1;
        assert!(!positions.contains(&unmatched));
        assert!(!bold_at(unmatched), "unmatched bytes must not render bold");
        assert!(
            syntax_at(unmatched),
            "unmatched bytes keep the syntax highlight"
        );
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_picker_navigates_and_dismisses_from_the_keyboard(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        bind_drill_navigation_keymap(cx);

        let mut t = symbol_test(None, cx);
        let buffer_id = t.buffer_id;
        let picker = t.picker.clone();
        let cx = &mut t.cx;

        t.editor.update(cx, |editor, cx| {
            editor.open_breadcrumb_symbol_navigation(buffer_id, None, cx);
            editor.watch_breadcrumb_symbol_dismissal(&picker, buffer_id, None, cx);
        });

        let dismissed = Rc::new(Cell::new(false));
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&t.picker, {
                let dismissed = dismissed.clone();
                move |_, _: &DismissEvent, _| dismissed.set(true)
            })
        });

        t.picker.update_in(cx, |picker, window, cx| {
            window.focus(&picker.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("down");
        t.picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker
                    .delegate
                    .item_at(picker.delegate.core.selected_index)
                    .map(|item| item.text.as_ref()),
                Some("impl Alpha"),
            );
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        t.editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let cursor = editor.selections.newest::<Point>(&snapshot).head();
            assert_eq!(
                cursor,
                Point::new(4, 5),
                "confirming the selected row puts the cursor on the impl's name, not its start"
            );
        });
        assert!(dismissed.get(), "confirming a row dismisses the popover");
        t.editor.read_with(cx, |editor, _| {
            assert!(
                editor.breadcrumb_symbol_navigation().is_none(),
                "the dismissal must clear the navigation session confirm just ended"
            );
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_select_parent_with_a_target_navigates_the_editor(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let mut t = symbol_test(Some("fn gamma"), cx);
        let cx = &mut t.cx;

        let result = t.picker.update_in(cx, |picker, window, cx| {
            picker.delegate.select_parent(window, cx)
        });
        cx.run_until_parked();

        assert_eq!(
            result,
            Some(String::new()),
            "select_parent with a target segment consumes the key and resets the query"
        );
        t.editor.read_with(cx, |editor, _| {
            let navigation = editor
                .breadcrumb_symbol_navigation()
                .expect("select_parent with a target segment navigates the editor's symbol state");
            assert!(navigation.navigated);
            assert_eq!(
                navigation
                    .active_item
                    .as_ref()
                    .map(|item| item.text.as_ref()),
                Some("impl Alpha"),
                "the navigation lands on the target's outline parent"
            );
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_select_parent_without_a_target_or_parent_dir_is_a_noop(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let mut t = symbol_test(None, cx);
        let cx = &mut t.cx;

        let result = t.picker.update_in(cx, |picker, window, cx| {
            picker.delegate.select_parent(window, cx)
        });
        assert!(
            result.is_none(),
            "the file segment with no parent directory has nowhere to go"
        );
        t.editor.read_with(cx, |editor, _| {
            assert!(
                editor.breadcrumb_navigation().is_none(),
                "the parent-directory fallback navigates the directory axis; a noop must not"
            );
            assert!(editor.breadcrumb_symbol_navigation().is_none());
        });
    }

    #[gpui::test]
    async fn test_select_parent_and_child_do_not_drill_with_a_query(cx: &mut TestAppContext) {
        init_test(cx);
        bind_drill_navigation_keymap(cx);

        let mut t = symbol_test(Some("fn gamma"), cx);
        let cx = &mut t.cx;
        cx.run_until_parked();

        t.picker.update_in(cx, |picker, window, cx| {
            window.focus(&picker.focus_handle(cx), cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("a l p h a");
        cx.run_until_parked();
        t.picker.update(cx, |picker, cx| {
            assert_eq!(picker.query(cx), "alpha");
        });

        // A non-empty query leaves left/right for the caret, even with a target set: a swallowed
        // key would leave the typed letter appended at the end instead of landing at the caret.
        cx.simulate_keystrokes("left");
        cx.simulate_keystrokes("z");
        cx.run_until_parked();
        t.picker.update(cx, |picker, cx| {
            assert_eq!(picker.query(cx), "alphza");
        });

        cx.simulate_keystrokes("right");
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        t.picker.update(cx, |picker, cx| {
            assert_eq!(picker.query(cx), "alphzay");
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_select_child_drills_into_the_selected_symbols_children(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        bind_drill_navigation_keymap(cx);

        let mut t = symbol_test(None, cx);
        let buffer_id = t.buffer_id;
        let cx = &mut t.cx;
        cx.run_until_parked();

        t.picker.read_with(cx, |picker, _| {
            assert_eq!(
                picker
                    .delegate
                    .item_at(picker.delegate.core.selected_index)
                    .map(|item| item.text.as_ref()),
                Some("struct Alpha"),
            );
        });

        t.picker.update_in(cx, |picker, window, cx| {
            window.focus(&picker.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("right");
        cx.run_until_parked();

        t.picker.update(cx, |picker, cx| {
            assert_eq!(
                picker.query(cx),
                "",
                "drilling resets the query for the reopened listing"
            );
        });
        t.editor.read_with(cx, |editor, cx| {
            let navigation = editor
                .breadcrumb_symbol_navigation()
                .expect("right drills into the selected symbol");
            assert!(navigation.navigated);
            let active_item = navigation
                .active_item
                .as_ref()
                .expect("the drilled symbol becomes the navigation target");
            assert_eq!(active_item.text.as_ref(), "struct Alpha");
            let children = editor
                .breadcrumb_symbol_menu_items(buffer_id, Some(active_item), cx)
                .iter()
                .map(|item| item.text.to_string())
                .collect::<Vec<_>>();
            assert_eq!(
                children,
                ["one"],
                "the reopened popover lists the drilled symbol's children"
            );
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_menu_outcome_is_not_ready_before_prefetch(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let mut fixture =
            editor_fixture::<BreadcrumbSymbolPicker>(SOURCE, Some(language::rust_lang()), cx);
        let editor = fixture.editor.clone();
        let cx = &mut fixture.cx;
        let buffer_id = singleton_buffer_id(&editor, cx);

        editor.read_with(cx, |editor, cx| {
            assert_eq!(
                breadcrumb_symbol_menu_outcome(editor, buffer_id, None, cx),
                BreadcrumbSymbolMenuOutcome::NotReady,
                "the outline has not been fetched yet, so the picker's items are not known; \
                 the caller kicks a prefetch and opens the popover in a loading state anyway"
            );
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_menu_outcome_is_empty_when_genuinely_empty(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        // A resolved language with no outline query still answers, just with nothing.
        let mut fixture = editor_fixture::<BreadcrumbSymbolPicker>(
            "alpha\nbeta\ngamma\n",
            Some(language::PLAIN_TEXT.clone()),
            cx,
        );
        let editor = fixture.editor.clone();
        let cx = &mut fixture.cx;
        let buffer_id = singleton_buffer_id(&editor, cx);
        cx.run_until_parked();

        editor.update(cx, |editor, cx| {
            editor.prefetch_breadcrumb_outline(buffer_id, cx);
        });
        cx.run_until_parked();

        editor.read_with(cx, |editor, cx| {
            assert_eq!(
                breadcrumb_symbol_menu_outcome(editor, buffer_id, None, cx),
                BreadcrumbSymbolMenuOutcome::Empty,
                "a loaded outline with no items shows the popover's own empty state"
            );
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_menu_outcome_is_ready_with_the_resolved_items(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);

        let mut fixture = resolved_outline_fixture(cx);
        let editor = fixture.editor.clone();
        let cx = &mut fixture.cx;
        let buffer_id = singleton_buffer_id(&editor, cx);

        editor.read_with(cx, |editor, cx| {
            let expected = editor.breadcrumb_symbol_menu_items(buffer_id, None, cx);
            assert_eq!(
                expected
                    .iter()
                    .map(|item| item.text.as_ref())
                    .collect::<Vec<_>>(),
                ["struct Alpha", "impl Alpha"],
            );
            assert_eq!(
                breadcrumb_symbol_menu_outcome(editor, buffer_id, None, cx),
                BreadcrumbSymbolMenuOutcome::Ready(expected),
                "a resolved, non-empty outline hands the popover its items"
            );
        });
    }

    #[gpui::test]
    async fn test_breadcrumb_symbol_picker_fills_in_once_the_outline_resolves(
        cx: &mut TestAppContext,
    ) {
        use editor::test::editor_lsp_test_context::EditorLspTestContext;

        init_test(cx);

        let mut lsp_cx =
            EditorLspTestContext::new_rust(lsp::ServerCapabilities::default(), cx).await;
        lsp_cx.set_state("struct Foo {}\n\nimpl Foo {\n    fn baˇr() {}\n}\n");
        lsp_cx.run_until_parked();

        let editor = lsp_cx.editor.clone();
        let buffer_id = lsp_cx.update_editor(|editor, _window, cx| {
            editor
                .buffer()
                .read(cx)
                .as_singleton()
                .unwrap()
                .read(cx)
                .remote_id()
        });
        lsp_cx.update_editor(|editor, _window, cx| {
            editor.prefetch_breadcrumb_outline(buffer_id, cx);
        });

        let cx: &mut VisualTestContext = &mut lsp_cx;

        let picker = cx.update(|window, cx| {
            BreadcrumbSymbolDelegate::picker(
                editor.downgrade(),
                buffer_id,
                None,
                Vec::new(),
                true,
                None,
                window,
                cx,
            )
        });

        let loading_placeholder = cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.delegate.no_matches_text(window, cx))
        });
        assert_eq!(loading_placeholder, Some("Loading symbols…".into()));

        // `refresh` later reads `picker.query`, not the delegate's field, so it must be set here too.
        picker.update_in(cx, |picker, window, cx| {
            picker.set_query("struct", window, cx);
        });
        let still_loading_placeholder = cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.delegate.no_matches_text(window, cx))
        });
        assert_eq!(
            still_loading_placeholder,
            Some("Loading symbols…".into()),
            "loading wins over the typed query's own empty result"
        );

        cx.run_until_parked();

        picker.read_with(cx, |picker, _| {
            assert!(!picker.delegate.loading);
            assert_eq!(
                picker.delegate.match_count(),
                1,
                "the still-typed query filters the now-real symbols instead of listing all of them"
            );
            assert_eq!(
                picker.delegate.item_at(0).map(|item| item.text.as_ref()),
                Some("struct Foo"),
                "the one match left is the struct, not the impl block"
            );
        });
    }
    #[test]
    fn test_stale_match_emphasis_is_skipped_when_the_item_text_changed() {
        let syntax_highlight = HighlightStyle {
            color: Some(gpui::red()),
            ..Default::default()
        };
        let item = OutlineItem::<Anchor> {
            depth: 0,
            range: Anchor::Min..Anchor::Max,
            selection_range: Anchor::Min..Anchor::Max,
            source_range_for_text: Anchor::Min..Anchor::Max,
            text: "fn short".into(),
            highlight_ranges: vec![(0..2, syntax_highlight)],
            name_ranges: Vec::new(),
            body_range: None,
            annotation_range: None,
        };
        let stale = StringMatch {
            candidate_id: 0,
            score: 0.,
            positions: vec![20, 21],
            string: "fn a_name_much_longer_than_the_refreshed_item".to_string(),
        };
        assert_eq!(
            match_emphasized_highlights(
                &item,
                &flatten_text_for_single_line_display(&item.text),
                &stale
            ),
            vec![(0..2, syntax_highlight)],
            "a stale match must render syntax highlights only"
        );

        let fresh = StringMatch {
            candidate_id: 0,
            score: 0.,
            positions: vec![3, 4],
            string: "fn short".to_string(),
        };
        assert!(
            match_emphasized_highlights(
                &item,
                &flatten_text_for_single_line_display(&item.text),
                &fresh
            )
            .iter()
            .any(|(_, style)| style.font_weight == Some(FontWeight::BOLD)),
            "a match for the current text keeps its bold emphasis"
        );
    }
}
