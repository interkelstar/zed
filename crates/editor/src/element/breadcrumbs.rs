//! Breadcrumb path and symbol navigation.

use std::sync::OnceLock;

use super::*;

mod layout;
mod outline;
mod path;

pub(crate) use layout::BreadcrumbSegmentKind;
use layout::{
    align_symbol_segments, classify_breadcrumb_segment_kinds, hard_cap_breadcrumb_middle_segments,
};
use layout::{
    breadcrumb_layout_plan_for_expansion, breadcrumb_layout_plan_width, plan_breadcrumb_layout,
};
pub(crate) use outline::outline_parents;
pub use outline::{child_outline_indices, sibling_outline_indices, top_level_outline_indices};
use path::breadcrumb_path_is_navigable;
pub use path::{
    BreadcrumbDirectoryEntry, BreadcrumbDirectoryListingSettings, breadcrumb_diagnostic_severity,
    breadcrumb_directory_entries,
};
pub(crate) use path::{breadcrumb_path_segments, breadcrumb_segment_copy_path};

/// Popover handle registered by `breadcrumb_picker`, type-erased to avoid a dependency cycle.
pub trait ErasedBreadcrumbPopoverHandle: 'static {
    fn hide(&self, cx: &mut App);
    fn show(&self, window: &mut Window, cx: &mut App);
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Renderers registered by `breadcrumb_picker`; a `OnceLock` severs the dependency cycle.
pub struct BreadcrumbPickerRenderers {
    pub directory: fn(
        WeakEntity<Editor>,
        WeakEntity<Workspace>,
        WorktreeId,
        Arc<RelPath>,
        Option<Arc<RelPath>>,
        bool,
        Rc<dyn ErasedBreadcrumbPopoverHandle>,
        gpui::AnyElement,
        usize,
    ) -> gpui::AnyElement,
    pub symbol: fn(
        WeakEntity<Editor>,
        BufferId,
        Option<OutlineItem<Anchor>>,
        Option<(WorktreeId, Arc<RelPath>)>,
        bool,
        Rc<dyn ErasedBreadcrumbPopoverHandle>,
        gpui::AnyElement,
        usize,
    ) -> gpui::AnyElement,
    pub popover_handle: fn() -> Rc<dyn ErasedBreadcrumbPopoverHandle>,
    pub symbol_popover_handle: fn() -> Rc<dyn ErasedBreadcrumbPopoverHandle>,
}

pub static BREADCRUMB_PICKER_RENDERERS: OnceLock<BreadcrumbPickerRenderers> = OnceLock::new();

#[derive(Clone, Debug)]
pub(crate) enum BreadcrumbSegmentTarget {
    Symbol {
        buffer_id: BufferId,
        item: Option<OutlineItem<Anchor>>,
        is_active_segment: bool,
    },
    Directory {
        worktree_id: WorktreeId,
        path: Arc<RelPath>,
        active_path: Option<Arc<RelPath>>,
        is_active_segment: bool,
    },
}

/// The single-byte replacement keeps byte-offset highlight ranges valid.
pub fn flatten_text_for_single_line_display(text: &str) -> String {
    text.replace('\n', " ")
}

struct PreparedBreadcrumbSegment {
    kind: BreadcrumbSegmentKind,
    label: HighlightedText,
    target: Option<BreadcrumbSegmentTarget>,
    /// Precomputed: the `'static` `BreadcrumbsRow` can't hold `active_item`.
    dirty_filename_style: bool,
    icon: Option<SharedString>,
    /// Diagnostics tint the icon and git status owns the label, as in the project panel.
    icon_color: Color,
    label_color: Color,
}

/// Measured once per render; `shape_line` is cached by text and font.
struct BreadcrumbSegmentMetrics {
    widths: Vec<Pixels>,
    ellipsis_width: Pixels,
}

/// Measured with the bold dirty-file style, which is wider than the base weight.
fn segment_text_runs(
    segment: &PreparedBreadcrumbSegment,
    text: &str,
    text_style: &gpui::TextStyle,
) -> Vec<gpui::TextRun> {
    let Some(filename_offset) = segment
        .dirty_filename_style
        .then(|| dirty_filename_offset(&segment.label))
        .flatten()
    else {
        return vec![text_style.to_run(text.len())];
    };

    let mut bold_style = text_style.clone();
    bold_style.font_weight = FontWeight::BOLD;
    if filename_offset == 0 {
        return vec![bold_style.to_run(text.len())];
    }
    vec![
        text_style.to_run(filename_offset),
        bold_style.to_run(text.len() - filename_offset),
    ]
}

/// A custom `Element`: how many segments fit depends on the row's real width.
struct BreadcrumbsRow {
    segments: Vec<PreparedBreadcrumbSegment>,
    editor: Option<WeakEntity<Editor>>,
    /// Set by clicking the ellipsis: renders every segment instead of dropping any.
    expanded: bool,
    file_outlives_symbols: bool,
}

const BREADCRUMB_SEGMENT_GROUP: &str = "breadcrumb-segment";

const BREADCRUMB_LABEL_PADDING: Pixels = px(4.);

const BREADCRUMB_ICON_SIZE: IconSize = IconSize::Small;

fn breadcrumb_file_icon(file_path: Option<&RelPath>, cx: &App) -> Option<SharedString> {
    if !BreadcrumbDirectoryListingSettings::get_global(cx).file_icons {
        return None;
    }
    file_icons::FileIcons::get_icon(file_path?.as_std_path(), cx)
}

/// Keeps at most one file icon on screen: the tab-bar-hidden prefix already renders one.
fn breadcrumb_segment_file_icon(
    icon: Option<SharedString>,
    prefix_present: bool,
) -> Option<SharedString> {
    if prefix_present { None } else { icon }
}

fn breadcrumb_separator_width(window: &Window) -> Pixels {
    IconSize::XSmall.rems().to_pixels(window.rem_size())
}

impl BreadcrumbsRow {
    fn effective_text_style(&self, window: &Window) -> gpui::TextStyle {
        window.text_style()
    }

    fn measure(&self, window: &mut Window) -> BreadcrumbSegmentMetrics {
        let text_style = self.effective_text_style(window);
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let gap = window.rem_size() * 0.25;

        let arrow_width = breadcrumb_separator_width(window);

        let ellipsis_run = text_style.to_run("⋯".len());
        let ellipsis_label_width = window
            .text_system()
            .shape_line("⋯".into(), font_size, &[ellipsis_run], None)
            .width();
        let ellipsis_width =
            ellipsis_label_width + BREADCRUMB_LABEL_PADDING * 2. + arrow_width + gap * 2.;

        let widths = self
            .segments
            .iter()
            .map(|segment| {
                let text = flatten_text_for_single_line_display(&segment.label.text);
                let runs = segment_text_runs(segment, &text, &text_style);
                let label_width = window
                    .text_system()
                    .shape_line(text.into(), font_size, &runs, None)
                    .width();
                let icon_width = if segment.icon.is_some() {
                    BREADCRUMB_ICON_SIZE.rems().to_pixels(window.rem_size()) + gap
                } else {
                    Pixels::ZERO
                };
                icon_width + label_width + BREADCRUMB_LABEL_PADDING * 2. + arrow_width + gap * 2.
            })
            .collect();

        BreadcrumbSegmentMetrics {
            widths,
            ellipsis_width,
        }
    }

    /// Positions in the final rendered sequence, not the raw segment index.
    fn with_separator(
        &self,
        position: usize,
        last_position: usize,
        content: gpui::AnyElement,
        interactive: bool,
        cx: &App,
    ) -> gpui::AnyElement {
        // The separator stays clickable but isn't part of the segment's name.
        let label = div()
            .px(BREADCRUMB_LABEL_PADDING)
            .rounded_sm()
            // Excerpt headers have no dropdowns, so no hover highlight.
            .when(interactive, |this| {
                this.group_hover(BREADCRUMB_SEGMENT_GROUP, |style| {
                    style.bg(cx.theme().colors().ghost_element_hover)
                })
            })
            .child(content);

        if position == last_position {
            return label.into_any_element();
        }
        h_flex()
            .gap_1()
            .child(label)
            .child(
                // Nudged down a pixel to sit on lowercase text's visual centre.
                div().relative().top(px(2.)).child(
                    Icon::new(IconName::ChevronRight)
                        .size(IconSize::XSmall)
                        .color(Color::Placeholder),
                ),
            )
            .into_any_element()
    }

    fn wrap_segment(&self, element: gpui::AnyElement) -> gpui::AnyElement {
        div()
            .group(BREADCRUMB_SEGMENT_GROUP)
            .child(element)
            .into_any_element()
    }

    fn render_segment(
        &self,
        index: usize,
        position: usize,
        last_position: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let segment = &self.segments[index];
        let mut text_style = self.effective_text_style(window);
        text_style.color = segment.label_color.color(cx);

        let text = if segment.dirty_filename_style
            && let Some(styled_element) =
                apply_dirty_filename_style(&segment.label, &text_style, cx)
        {
            styled_element
        } else {
            StyledText::new(flatten_text_for_single_line_display(&segment.label.text))
                .with_default_highlights(&text_style, segment.label.highlights.clone())
                .into_any()
        };

        let content = match &segment.icon {
            Some(icon) => h_flex()
                .gap_1()
                .child(
                    // The same optical nudge the separator chevron gets.
                    div().relative().top(px(2.)).child(
                        Icon::from_path(icon.clone())
                            .color(segment.icon_color)
                            .size(BREADCRUMB_ICON_SIZE),
                    ),
                )
                .child(text)
                .into_any_element(),
            None => text,
        };
        let content =
            if let (Some(target), Some(editor)) = (segment.target.clone(), self.editor.clone()) {
                div()
                    .on_mouse_down(gpui::MouseButton::Right, move |_, _, cx| {
                        let Some(editor) = editor.upgrade() else {
                            return;
                        };
                        let path = editor.update(cx, |editor, cx| {
                            resolve_breadcrumb_segment_copy_path(&target, editor, cx)
                        });
                        if let Some(path) = path {
                            cx.write_to_clipboard(ClipboardItem::new_string(path));
                        }
                        // Otherwise the ancestor container's handler runs later in the bubble phase and overwrites this.
                        cx.stop_propagation();
                    })
                    .child(content)
                    .into_any_element()
            } else {
                content
            };
        let interactive = segment.target.is_some() && self.editor.is_some();
        let label = self.with_separator(position, last_position, content, interactive, cx);

        let Some(renderers) = BREADCRUMB_PICKER_RENDERERS.get() else {
            return label;
        };
        let element =
            match (segment.target.clone(), self.editor.clone()) {
                (
                    Some(BreadcrumbSegmentTarget::Symbol {
                        buffer_id,
                        item,
                        is_active_segment,
                    }),
                    Some(editor),
                ) => {
                    let parent_dir = self.segments[..index].iter().rev().find_map(|segment| {
                        match &segment.target {
                            Some(BreadcrumbSegmentTarget::Directory {
                                worktree_id, path, ..
                            }) => Some((*worktree_id, path.clone())),
                            _ => None,
                        }
                    });
                    let Some(upgraded_editor) = editor.upgrade() else {
                        return label;
                    };
                    let Some(shared_popover_handle) =
                        upgraded_editor.read(cx).breadcrumb_symbol_popover_handle()
                    else {
                        return label;
                    };
                    (renderers.symbol)(
                        editor,
                        buffer_id,
                        item,
                        parent_dir,
                        is_active_segment,
                        shared_popover_handle,
                        label,
                        index,
                    )
                }
                (
                    Some(BreadcrumbSegmentTarget::Directory {
                        worktree_id,
                        path,
                        active_path,
                        is_active_segment,
                    }),
                    Some(editor),
                ) => {
                    let Some(upgraded_editor) = editor.upgrade() else {
                        return label;
                    };
                    let Some(workspace) = upgraded_editor
                        .read(cx)
                        .workspace()
                        .map(|workspace| workspace.downgrade())
                    else {
                        return label;
                    };
                    let Some(shared_popover_handle) =
                        upgraded_editor.read(cx).breadcrumb_popover_handle()
                    else {
                        return label;
                    };
                    (renderers.directory)(
                        editor,
                        workspace,
                        worktree_id,
                        path,
                        active_path,
                        is_active_segment,
                        shared_popover_handle,
                        label,
                        index,
                    )
                }
                _ => return label,
            };
        self.wrap_segment(element)
    }

    fn render_ellipsis(&self, position: usize, last_position: usize, cx: &App) -> gpui::AnyElement {
        let content = Label::new("⋯").color(Color::Placeholder).into_any_element();
        let label = self.with_separator(position, last_position, content, true, cx);
        let Some(editor) = self.editor.clone() else {
            return label;
        };
        let element = div()
            // Protecting an anchored segment can split the dropped run in two.
            .id(("breadcrumb-ellipsis", position))
            .cursor_pointer()
            .tooltip(Tooltip::text("Show Full Path"))
            .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                if let Some(editor) = editor.upgrade() {
                    editor.update(cx, |editor, cx| editor.expand_breadcrumb_trail(cx));
                }
            })
            .child(label)
            .into_any_element();
        self.wrap_segment(element)
    }

    /// The segment a popover is currently open under, if any: `plan_breadcrumb_layout` must never collapse it.
    fn anchored_segment_index(&self) -> Option<usize> {
        self.segments.iter().position(|segment| {
            matches!(
                segment.target,
                Some(BreadcrumbSegmentTarget::Directory {
                    is_active_segment: true,
                    ..
                }) | Some(BreadcrumbSegmentTarget::Symbol {
                    is_active_segment: true,
                    ..
                })
            )
        })
    }
}

fn resolve_breadcrumb_segment_copy_path(
    target: &BreadcrumbSegmentTarget,
    editor: &Editor,
    cx: &mut Context<Editor>,
) -> Option<String> {
    let worktree_abs_path = match target {
        BreadcrumbSegmentTarget::Directory { worktree_id, .. } => editor
            .project()
            .and_then(|project| project.read(cx).worktree_for_id(*worktree_id, cx))
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf()),
        BreadcrumbSegmentTarget::Symbol { .. } => None,
    };
    let file_abs_path = editor.target_file_abs_path(cx);
    let symbol_line = match target {
        BreadcrumbSegmentTarget::Symbol {
            item: Some(item), ..
        } => {
            // Buffer coordinates, not multibuffer: expanded diff hunks shift multibuffer rows.
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            snapshot
                .anchor_to_buffer_anchor(item.range.start)
                .map(|(anchor, buffer)| text::ToPoint::to_point(&anchor, buffer).row + 1)
        }
        _ => None,
    };
    breadcrumb_segment_copy_path(target, worktree_abs_path, file_abs_path, symbol_line)
}

/// Called when the layout drops the anchored segment: otherwise the popover stays deployed with no trigger left to dismiss it.
fn dismiss_orphaned_breadcrumb_popover(
    editor: &Entity<Editor>,
    target: &BreadcrumbSegmentTarget,
    cx: &mut App,
) {
    match target {
        BreadcrumbSegmentTarget::Directory {
            worktree_id, path, ..
        } => {
            if let Some(handle) = editor.read(cx).breadcrumb_popover_handle() {
                handle.hide(cx);
            }
            editor.update(cx, |editor, cx| {
                editor.clear_breadcrumb_navigation(*worktree_id, path, cx);
            });
        }
        BreadcrumbSegmentTarget::Symbol {
            buffer_id, item, ..
        } => {
            if let Some(handle) = editor.read(cx).breadcrumb_symbol_popover_handle() {
                handle.hide(cx);
            }
            editor.update(cx, |editor, cx| {
                editor.clear_breadcrumb_symbol_navigation(*buffer_id, item.as_ref(), cx);
            });
        }
    }
}

struct BreadcrumbsRowPrepaintState {
    children: Vec<gpui::AnyElement>,
}

impl gpui::IntoElement for BreadcrumbsRow {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for BreadcrumbsRow {
    type RequestLayoutState = BreadcrumbSegmentMetrics;
    type PrepaintState = BreadcrumbsRowPrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let metrics = self.measure(window);
        let natural_width = metrics
            .widths
            .iter()
            .fold(Pixels::ZERO, |total, width| total + *width);
        let line_height = window.text_style().line_height_in_pixels(window.rem_size());

        let widths = metrics.widths.clone();
        let ellipsis_width = metrics.ellipsis_width;
        let kinds: Vec<BreadcrumbSegmentKind> = self.segments.iter().map(|s| s.kind).collect();
        let anchored_index = self.anchored_segment_index();
        let expanded = self.expanded;
        let file_outlives_symbols = self.file_outlives_symbols;

        // Answering `MinContent` with the whole trail would stop the parent offering less.
        let mut style = Style::default();
        style.min_size.width = px(0.).into();
        // Expanded the row must outgrow its parent, or the scroll container has nothing to scroll.
        if expanded {
            style.flex_shrink = 0.;
        }

        let layout_id = window.request_measured_layout(
            style,
            move |known_dimensions, available_space, _window, _cx| {
                // Expanded ignores the offered width, including a resolved one: the whole point
                // is to outgrow the container so it can be scrolled.
                let width = if expanded {
                    natural_width
                } else {
                    known_dimensions
                        .width
                        .unwrap_or(match available_space.width {
                            AvailableSpace::Definite(available_width) => {
                                let plan = plan_breadcrumb_layout(
                                    &widths,
                                    &kinds,
                                    ellipsis_width,
                                    available_width,
                                    anchored_index,
                                    file_outlives_symbols,
                                );
                                breadcrumb_layout_plan_width(&widths, &plan, ellipsis_width)
                            }
                            AvailableSpace::MinContent => widths
                                .last()
                                .copied()
                                .unwrap_or(ellipsis_width)
                                .max(ellipsis_width),
                            AvailableSpace::MaxContent => natural_width,
                        })
                };
                let height = known_dimensions.height.unwrap_or(line_height);
                size(width, height)
            },
        );

        (layout_id, metrics)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        metrics: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let kinds: Vec<BreadcrumbSegmentKind> = self.segments.iter().map(|s| s.kind).collect();
        let anchored_index = self.anchored_segment_index();
        let plan =
            breadcrumb_layout_plan_for_expansion(self.expanded, kinds.len()).unwrap_or_else(|| {
                plan_breadcrumb_layout(
                    &metrics.widths,
                    &kinds,
                    metrics.ellipsis_width,
                    bounds.size.width,
                    anchored_index,
                    self.file_outlives_symbols,
                )
            });

        if let Some(anchored_index) = anchored_index
            && !plan.visible.contains(&anchored_index)
            && let Some(target) = self
                .segments
                .get(anchored_index)
                .and_then(|segment| segment.target.clone())
            && let Some(editor) = self.editor.as_ref().and_then(WeakEntity::upgrade)
            // Reanchoring drops and re-shows the popover itself; dismissing mid-flight fights it.
            && !editor.read(cx).breadcrumb_reanchoring
        {
            dismiss_orphaned_breadcrumb_popover(&editor, &target, cx);
        }

        enum FinalItem {
            Segment(usize),
            Ellipsis,
        }

        let segment_count = kinds.len();
        let mut sequence = Vec::with_capacity(plan.visible.len() + plan.ellipses.len());
        let mut index = 0;
        while index < segment_count {
            if let Some(range) = plan.ellipses.iter().find(|range| range.start == index) {
                sequence.push(FinalItem::Ellipsis);
                index = range.end;
            } else {
                sequence.push(FinalItem::Segment(index));
                index += 1;
            }
        }

        let last_position = sequence.len().saturating_sub(1);
        let gap = window.rem_size() * 0.25;
        let mut x = bounds.origin.x;
        let mut children = Vec::with_capacity(sequence.len());
        for (position, item) in sequence.into_iter().enumerate() {
            let mut element = match item {
                FinalItem::Segment(index) => {
                    self.render_segment(index, position, last_position, window, cx)
                }
                FinalItem::Ellipsis => self.render_ellipsis(position, last_position, cx),
            };
            let available_space = size(
                AvailableSpace::MaxContent,
                AvailableSpace::Definite(bounds.size.height),
            );
            let element_size = element.layout_as_root(available_space, window, cx);
            element.prepaint_at(point(x, bounds.origin.y), window, cx);
            x += element_size.width + gap;
            children.push(element);
        }

        if let Some(editor) = self.editor.as_ref().and_then(WeakEntity::upgrade)
            && editor.read(cx).breadcrumb_pending_reanchor().is_some()
        {
            editor.update(cx, |editor, cx| {
                editor.reanchor_breadcrumb_popover(window, cx);
            });
        }

        BreadcrumbsRowPrepaintState { children }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for child in &mut prepaint.children {
            child.paint(window, cx);
        }
    }
}

pub fn render_breadcrumb_text(
    mut segments: Vec<HighlightedText>,
    prefix: Option<gpui::AnyElement>,
    active_item: &dyn ItemHandle,
    multibuffer_header: bool,
    cx: &App,
) -> gpui::AnyElement {
    // min_w_0: a flex item's minimum size defaults to its content's.
    let element = h_flex().flex_grow_1().min_w_0().text_ui(cx);

    let editor = active_item
        .downcast::<Editor>()
        .map(|editor| editor.downgrade());

    // The singleton's buffer id, so the path segment gets a menu outside any symbol.
    let mut symbol_segments: Vec<Option<BreadcrumbSegmentTarget>> = Vec::new();
    let mut file_segment_index = 0usize;
    let mut has_root_segment = false;
    let mut file_path_for_icon: Option<Arc<RelPath>> = None;
    let mut file_status = None;
    let mut diagnostic_severity = None;

    if !multibuffer_header
        && let Some(editor_entity) = editor.as_ref().and_then(WeakEntity::upgrade)
    {
        let editor_ref = editor_entity.read(cx);
        if let Some(buffer) = editor_ref.buffer().read(cx).as_singleton() {
            let buffer_id = buffer.read(cx).remote_id();
            let mut path_split = false;

            let real_project_path = active_item.project_path(cx);
            file_path_for_icon = real_project_path
                .as_ref()
                .map(|project_path| project_path.path.clone());
            let listing_settings = BreadcrumbDirectoryListingSettings::get_global(cx);
            file_status = listing_settings.git_status.then(|| ()).and_then(|_| {
                editor_ref
                    .project()
                    .zip(real_project_path.as_ref())
                    .and_then(|(project, project_path)| {
                        project.read(cx).project_path_git_status(project_path, cx)
                    })
            });
            diagnostic_severity = editor_ref
                .project()
                .zip(real_project_path.as_ref())
                .and_then(|(project, project_path)| {
                    path::breadcrumb_diagnostic_severity(
                        project.read(cx),
                        project_path,
                        listing_settings.show_diagnostics,
                        cx,
                    )
                });
            // While set, the bar shows that directory's path instead of the file's.
            let navigation = editor_ref.breadcrumb_navigation().cloned();
            let navigated = navigation
                .as_ref()
                .is_some_and(|navigation| navigation.navigated);
            let active_segment = navigation
                .as_ref()
                .map(|navigation| navigation.active_path.clone());

            let symbol_navigation = editor_ref.breadcrumb_symbol_navigation().cloned();
            let file_segment_active = symbol_navigation.as_ref().is_some_and(|navigation| {
                navigation.buffer_id == buffer_id && navigation.active_item.is_none()
            });

            let is_navigable = breadcrumb_path_is_navigable(
                real_project_path.is_some(),
                real_project_path.as_ref().and_then(|project_path| {
                    editor_ref
                        .project()
                        .and_then(|project| {
                            project
                                .read(cx)
                                .worktree_for_id(project_path.worktree_id, cx)
                        })
                        .map(|worktree| worktree.read(cx).is_single_file())
                }),
            );

            // The root segment keeps sibling top-level directories reachable from the root.
            if is_navigable
                && !segments.is_empty()
                && let Some(project) = editor_ref.project()
            {
                let split = if let Some(navigation) = navigation
                    .as_ref()
                    .filter(|navigation| navigation.navigated)
                {
                    project
                        .read(cx)
                        .worktree_for_id(navigation.worktree_id, cx)
                        .map(|worktree| {
                            breadcrumb_path_segments(
                                navigation.worktree_id,
                                worktree.read(cx).root_name_str(),
                                &navigation.active_path,
                                real_project_path.as_ref().map(|path| path.path.clone()),
                                None,
                                active_segment.as_deref(),
                                false,
                            )
                        })
                } else if let Some(project_path) = real_project_path.as_ref()
                    && let Some(worktree) = project
                        .read(cx)
                        .worktree_for_id(project_path.worktree_id, cx)
                {
                    Some(breadcrumb_path_segments(
                        project_path.worktree_id,
                        worktree.read(cx).root_name_str(),
                        &project_path.path,
                        Some(project_path.path.clone()),
                        Some(buffer_id),
                        active_segment.as_deref(),
                        file_segment_active,
                    ))
                } else {
                    None
                };

                if let Some((path_labels, path_targets)) = split {
                    file_segment_index = path_labels.len() - 1;
                    let replace_range = if navigated { 0..segments.len() } else { 0..1 };
                    segments.splice(replace_range, path_labels);
                    symbol_segments = path_targets;
                    path_split = true;
                    has_root_segment = true;
                }
            }

            if !path_split {
                symbol_segments.push(file_segment_symbol_target(buffer_id, file_segment_active));
            }

            // Directory navigation replaces the whole bar; symbol segments don't apply.
            if !navigated {
                let symbol_navigated = symbol_navigation.as_ref().is_some_and(|navigation| {
                    navigation.buffer_id == buffer_id && navigation.navigated
                });

                if symbol_navigated {
                    let trail = symbol_navigation
                        .as_ref()
                        .and_then(|navigation| navigation.active_item.as_ref())
                        .map(|item| editor_ref.breadcrumb_symbol_trail(buffer_id, item, cx))
                        .unwrap_or_default();
                    // The incoming labels carry the cursor's symbol trail; navigation replaces it.
                    segments.truncate(file_segment_index + 1);
                    segments.extend(trail.iter().map(|item| HighlightedText {
                        text: item.text.clone(),
                        highlights: item.highlight_ranges.clone(),
                    }));
                    let last_index = trail.len().saturating_sub(1);
                    symbol_segments.extend(trail.into_iter().enumerate().map(|(index, item)| {
                        Some(BreadcrumbSegmentTarget::Symbol {
                            buffer_id,
                            item: Some(item),
                            is_active_segment: index == last_index,
                        })
                    }));
                } else {
                    let ancestors = editor_ref
                        .outline_symbols_at_cursor
                        .as_ref()
                        .filter(|(id, _)| *id == buffer_id)
                        .map(|(_, ancestors)| ancestors.as_slice())
                        .unwrap_or_default();
                    let active_range = symbol_navigation
                        .as_ref()
                        .filter(|navigation| navigation.buffer_id == buffer_id)
                        .and_then(|navigation| navigation.active_item.as_ref())
                        .map(|item| item.range.clone());
                    symbol_segments.extend(ancestors.iter().cloned().map(|item| {
                        let is_active_segment = active_range.as_ref() == Some(&item.range);
                        Some(BreadcrumbSegmentTarget::Symbol {
                            buffer_id,
                            item: Some(item),
                            is_active_segment,
                        })
                    }));
                }
            }
        }
    }

    let symbol_segments = align_symbol_segments(&segments, symbol_segments);
    let kinds =
        classify_breadcrumb_segment_kinds(segments.len(), file_segment_index, has_root_segment);
    let (segments, symbol_segments, kinds, file_segment_index) =
        hard_cap_breadcrumb_middle_segments(segments, symbol_segments, kinds, file_segment_index);

    let file_icon = breadcrumb_segment_file_icon(
        breadcrumb_file_icon(file_path_for_icon.as_deref(), cx),
        prefix.is_some(),
    );
    let file_status_color = crate::element::file_status_label_color(file_status);
    let file_icon_color =
        crate::items::entry_diagnostic_aware_icon_decoration_and_color(diagnostic_severity)
            .map(|(_, color)| color)
            .unwrap_or(Color::Muted);

    let tab_bar_hidden = !workspace::TabBarSettings::get_global(cx).show;
    let apply_dirty_filename_style = tab_bar_hidden && active_item.is_dirty(cx);

    let prepared_segments = segments
        .into_iter()
        .zip(symbol_segments)
        .zip(kinds)
        .enumerate()
        .map(|(index, ((label, target), kind))| {
            let is_file_segment = is_file_breadcrumb_segment(kind, target.as_ref());
            PreparedBreadcrumbSegment {
                kind,
                label,
                target,
                dirty_filename_style: apply_dirty_filename_style
                    && index == file_segment_index
                    && is_file_segment,
                icon: is_file_segment.then(|| file_icon.clone()).flatten(),
                icon_color: if is_file_segment {
                    file_icon_color
                } else {
                    Color::Muted
                },
                label_color: if is_file_segment {
                    file_status_color
                } else {
                    Color::Muted
                },
            }
        })
        .collect();

    let expanded = breadcrumb_row_is_expanded(
        multibuffer_header,
        editor
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|editor_entity| editor_entity.read(cx).breadcrumb_expanded()),
    );

    let row = BreadcrumbsRow {
        segments: prepared_segments,
        editor: editor.clone(),
        expanded,
        file_outlives_symbols: tab_bar_hidden,
    };

    let breadcrumbs_stack = if multibuffer_header {
        div()
            .min_w_0()
            .pl_2()
            .border_l_1()
            .border_color(cx.theme().colors().border.opacity(0.6))
            .child(row)
            .into_any_element()
    } else {
        h_flex()
            .id("breadcrumb-trail")
            .min_w_0()
            .overflow_x_scroll()
            .child(row)
            .into_any_element()
    };

    let breadcrumbs = if let Some(prefix) = prefix {
        h_flex()
            .min_w_0()
            .gap_1p5()
            .child(prefix)
            .child(breadcrumbs_stack)
            .into_any_element()
    } else {
        breadcrumbs_stack
    };

    let has_project_path = active_item.project_path(cx).is_some();

    match editor {
        Some(editor) => element
            .id("breadcrumb_container")
            // Not a `ButtonLike`: it renders `flex_none` and would never shrink.
            .child(
                h_flex()
                    .h(rems_from_px(22.))
                    .px_1()
                    // Dropping this lets the expanded row widen the toolbar instead of scrolling.
                    .min_w_0()
                    .child(breadcrumbs)
                    .when(!multibuffer_header && has_project_path, |this| {
                        this.on_mouse_down(gpui::MouseButton::Right, {
                            let editor = editor.clone();
                            move |_, _, cx| {
                                if let Some(abs_path) = editor.upgrade().and_then(|editor| {
                                    editor.update(cx, |editor, cx| editor.target_file_abs_path(cx))
                                }) && let Some(path_str) = abs_path.to_str()
                                {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        path_str.to_string(),
                                    ));
                                }
                            }
                        })
                    }),
            )
            .into_any_element(),
        None => element
            .h(rems_from_px(22.)) // Match the height and padding of the `ButtonLike` in the other arm.
            .pl_1()
            .child(breadcrumbs)
            .into_any_element(),
    }
}

/// Always a target, even when the path itself isn't navigable (single-file worktree, untitled):
/// the file segment still opens the outline picker.
fn file_segment_symbol_target(
    buffer_id: BufferId,
    is_active_segment: bool,
) -> Option<BreadcrumbSegmentTarget> {
    Some(BreadcrumbSegmentTarget::Symbol {
        buffer_id,
        item: None,
        is_active_segment,
    })
}

/// A multibuffer excerpt header has no scroll container, so an expanded row would overrun its neighbours.
fn breadcrumb_row_is_expanded(multibuffer_header: bool, expanded: bool) -> bool {
    !multibuffer_header && expanded
}

fn is_file_breadcrumb_segment(
    kind: BreadcrumbSegmentKind,
    target: Option<&BreadcrumbSegmentTarget>,
) -> bool {
    kind == BreadcrumbSegmentKind::File
        && matches!(
            target,
            Some(BreadcrumbSegmentTarget::Symbol { item: None, .. })
        )
}

/// Where the file name starts, shared between painting and measuring.
fn dirty_filename_offset(segment: &HighlightedText) -> Option<usize> {
    let filename = std::path::Path::new(segment.text.as_ref()).file_name()?;
    segment.text.rfind(filename.to_string_lossy().as_ref())
}

/// Bolds the filename in place, keeping whatever color `render_segment` already chose (git status).
fn dirty_filename_text_style(text_style: &gpui::TextStyle) -> gpui::TextStyle {
    let mut filename_style = text_style.clone();
    filename_style.font_weight = FontWeight::BOLD;
    filename_style
}

fn dirty_filename_highlight_style() -> gpui::HighlightStyle {
    gpui::HighlightStyle {
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    }
}

fn apply_dirty_filename_style(
    segment: &HighlightedText,
    text_style: &gpui::TextStyle,
    _cx: &App,
) -> Option<gpui::AnyElement> {
    let text = flatten_text_for_single_line_display(&segment.text);

    let filename_position = dirty_filename_offset(segment)?;

    if filename_position == 0 {
        let filename_style = dirty_filename_text_style(text_style);

        return Some(
            StyledText::new(text)
                .with_default_highlights(&filename_style, [])
                .into_any(),
        );
    }

    let highlight = vec![(
        filename_position..text.len(),
        dirty_filename_highlight_style(),
    )];
    Some(
        StyledText::new(text)
            .with_default_highlights(text_style, highlight)
            .into_any(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_breadcrumb_segment_file_icon_suppressed_when_a_prefix_is_present() {
        let icon: SharedString = "icons/file.svg".into();
        assert_eq!(
            breadcrumb_segment_file_icon(Some(icon.clone()), true),
            None,
            "a prefix icon already renders one"
        );
        assert_eq!(
            breadcrumb_segment_file_icon(Some(icon.clone()), false),
            Some(icon),
            "no prefix means the segment icon is the only one"
        );
        assert_eq!(breadcrumb_segment_file_icon(None, false), None);
    }

    #[test]
    fn test_flatten_text_for_single_line_display_preserves_byte_offsets() {
        // Byte-offset ranges must locate the same substring in both strings.
        let original = "fn outer() {\n    inner()\n}";
        let flattened = flatten_text_for_single_line_display(original);

        assert_eq!(flattened, "fn outer() {     inner() }");
        assert_eq!(flattened.len(), original.len());

        let inner_offset = original.find("inner").unwrap();
        assert_eq!(
            &flattened[inner_offset..inner_offset + "inner".len()],
            "inner",
        );
    }

    #[test]
    fn test_is_file_breadcrumb_segment_requires_the_bare_file_target() {
        let file_target = BreadcrumbSegmentTarget::Symbol {
            buffer_id: BufferId::new(1).unwrap(),
            item: None,
            is_active_segment: true,
        };
        assert!(is_file_breadcrumb_segment(
            BreadcrumbSegmentKind::File,
            Some(&file_target)
        ));

        let directory_target = BreadcrumbSegmentTarget::Directory {
            worktree_id: WorktreeId::from_usize(0),
            path: RelPath::empty().into(),
            active_path: None,
            is_active_segment: true,
        };
        assert!(
            !is_file_breadcrumb_segment(BreadcrumbSegmentKind::File, Some(&directory_target)),
            "a navigated bar can put a Directory at file_segment_index"
        );

        assert!(!is_file_breadcrumb_segment(
            BreadcrumbSegmentKind::Middle,
            Some(&file_target)
        ));
    }

    #[test]
    fn test_breadcrumb_row_is_expanded_stays_false_for_a_multibuffer_header() {
        assert!(
            !breadcrumb_row_is_expanded(true, true),
            "a header row has no scroll container to contain an expanded trail"
        );
        assert!(breadcrumb_row_is_expanded(false, true));
        assert!(!breadcrumb_row_is_expanded(false, false));
    }

    #[test]
    fn test_dirty_filename_text_style_only_changes_font_weight() {
        let mut base = gpui::TextStyle::default();
        base.color = gpui::red();

        let dirty = dirty_filename_text_style(&base);

        assert_eq!(dirty.color, base.color, "the git status color must survive");
        assert_eq!(dirty.font_weight, FontWeight::BOLD);
    }

    #[test]
    fn test_file_segment_symbol_target_is_set_even_when_not_navigable() {
        let buffer_id = BufferId::new(1).unwrap();

        let target = file_segment_symbol_target(buffer_id, false)
            .expect("the file segment always gets a target, navigable or not");
        assert!(matches!(
            target,
            BreadcrumbSegmentTarget::Symbol {
                item: None,
                is_active_segment: false,
                ..
            }
        ));
    }

    #[test]
    fn test_dirty_filename_highlight_style_carries_no_color() {
        let highlight = dirty_filename_highlight_style();

        assert_eq!(
            highlight.color, None,
            "color must fall through to the base run's"
        );
        assert_eq!(highlight.font_weight, Some(FontWeight::BOLD));
    }

    /// `editor` cannot depend on `breadcrumb_picker` for a real popover handle, so this covers
    /// only the navigation-state half of `dismiss_orphaned_breadcrumb_popover`.
    #[gpui::test]
    fn test_dismiss_orphaned_breadcrumb_popover_clears_directory_navigation(
        cx: &mut gpui::TestAppContext,
    ) {
        crate::editor_tests::init_test(cx, |_| {});

        let buffer = cx.new(|cx| language::Buffer::local("fn main() {}", cx));
        let buffer = cx.new(|cx| multi_buffer::MultiBuffer::singleton(buffer, cx));
        let editor_window =
            cx.add_window(|window, cx| crate::test::build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap();
        let worktree_id = WorktreeId::from_usize(0);
        let path = RelPath::empty().into_arc();

        editor.update(cx, |editor, cx| {
            editor.open_breadcrumb_navigation(worktree_id, path.clone(), cx);
        });
        editor.read_with(cx, |editor, _| {
            assert!(editor.breadcrumb_navigation().is_some());
        });

        let target = BreadcrumbSegmentTarget::Directory {
            worktree_id,
            path: path.clone(),
            active_path: None,
            is_active_segment: true,
        };
        cx.update(|cx| {
            dismiss_orphaned_breadcrumb_popover(&editor, &target, cx);
        });

        editor.read_with(cx, |editor, _| {
            assert!(
                editor.breadcrumb_navigation().is_none(),
                "a segment the layout could not keep must drop its navigation session"
            );
        });
    }

    fn breadcrumb_row_scroll_range(expanded: bool, cx: &mut gpui::TestAppContext) -> Pixels {
        crate::editor_tests::init_test(cx, |_| {});
        let scroll_handle = gpui::ScrollHandle::new();
        let window = cx.add_window({
            let scroll_handle = scroll_handle.clone();
            move |_, _| ScrollProbe {
                expanded,
                scroll_handle,
            }
        });
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
        scroll_handle.max_offset().x
    }

    struct ScrollProbe {
        expanded: bool,
        scroll_handle: gpui::ScrollHandle,
    }

    impl Render for ScrollProbe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let segments = (0..12)
                .map(|index| PreparedBreadcrumbSegment {
                    kind: BreadcrumbSegmentKind::Middle,
                    label: HighlightedText {
                        text: format!("directory-with-a-long-name-{index}").into(),
                        highlights: Vec::new(),
                    },
                    target: None,
                    dirty_filename_style: false,
                    icon: None,
                    icon_color: Color::Muted,
                    label_color: Color::Muted,
                })
                .collect();
            let row = BreadcrumbsRow {
                segments,
                editor: None,
                expanded: self.expanded,
                file_outlives_symbols: false,
            };
            // Mirrors the real chain, clipping box and nested rows included.
            h_flex().w(px(200.)).overflow_x_hidden().child(
                h_flex().flex_grow_1().min_w_0().child(
                    h_flex().min_w_0().child(
                        h_flex()
                            .id("breadcrumb-trail")
                            .min_w_0()
                            .overflow_x_scroll()
                            .track_scroll(&self.scroll_handle)
                            .child(row),
                    ),
                ),
            )
        }
    }

    #[gpui::test]
    fn test_expanded_breadcrumb_row_outgrows_its_container(cx: &mut gpui::TestAppContext) {
        assert_eq!(
            breadcrumb_row_scroll_range(false, cx),
            px(0.),
            "a collapsed row fits by dropping segments, so there is nothing to scroll"
        );
        assert!(
            breadcrumb_row_scroll_range(true, cx) > px(0.),
            "an expanded row must overflow its container for the scroll container to reach the tail"
        );
    }
}
