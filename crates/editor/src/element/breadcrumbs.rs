//! Breadcrumb path and symbol navigation.

use std::cell::RefCell;
use std::ops::Range;
use std::sync::OnceLock;

use gpui::Subscription;
use ui::{ScrollAxes, Scrollbars, WithScrollbar};

use super::*;
use crate::{BreadcrumbNavigation, BreadcrumbSymbolNavigation};

mod layout;
mod outline;
mod path;

pub(crate) use layout::BreadcrumbSegmentKind;
use layout::{
    BreadcrumbLayoutPlan, align_symbol_segments, classify_breadcrumb_segment_kinds,
    hard_cap_breadcrumb_middle_segments,
};
use layout::{breadcrumb_layout_plan_width, plan_breadcrumb_layout};
pub(crate) use outline::outline_parents;
pub use outline::{child_outline_indices, sibling_outline_indices, top_level_outline_indices};
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
    /// Horizontal padding the trigger wraps a segment in; the row measures with it or paints past its own bounds.
    pub segment_padding: Pixels,
}

pub static BREADCRUMB_PICKER_RENDERERS: OnceLock<BreadcrumbPickerRenderers> = OnceLock::new();

/// Which dropdown session is live; the `Reanchoring*` variants cover the popover moving to a new segment.
pub(crate) enum BreadcrumbPopover {
    Closed,
    Directory(BreadcrumbNavigation),
    Symbol(BreadcrumbSymbolNavigation),
    ReanchoringDirectory {
        navigation: BreadcrumbNavigation,
        pending: bool,
    },
    ReanchoringSymbol {
        navigation: BreadcrumbSymbolNavigation,
        pending: bool,
    },
}

/// Buffer-coordinate on purpose: multibuffer layout can change under the key.
struct SymbolTrailCache {
    buffer_id: BufferId,
    version: clock::Global,
    range: Range<Anchor>,
    trail: Vec<OutlineItem<text::Anchor>>,
}

pub(crate) struct BreadcrumbState {
    popover: BreadcrumbPopover,
    pub(crate) expanded: bool,
    /// Set for one frame by a mouse-down on the bar, so a dismiss from that click keeps the trail expanded.
    pub(crate) zone_mouse_down: bool,
    pub(crate) dismiss_subscription: Option<Subscription>,
    directory_popover_handle: Option<Rc<dyn ErasedBreadcrumbPopoverHandle>>,
    symbol_popover_handle: Option<Rc<dyn ErasedBreadcrumbPopoverHandle>>,
    symbol_trail_cache: RefCell<Option<SymbolTrailCache>>,
}

impl Default for BreadcrumbState {
    fn default() -> Self {
        let renderers = BREADCRUMB_PICKER_RENDERERS.get();
        Self {
            popover: BreadcrumbPopover::Closed,
            expanded: false,
            zone_mouse_down: false,
            dismiss_subscription: None,
            directory_popover_handle: renderers.map(|renderers| (renderers.popover_handle)()),
            symbol_popover_handle: renderers.map(|renderers| (renderers.symbol_popover_handle)()),
            symbol_trail_cache: RefCell::new(None),
        }
    }
}

impl BreadcrumbState {
    pub(crate) fn directory_navigation(&self) -> Option<&BreadcrumbNavigation> {
        match &self.popover {
            BreadcrumbPopover::Directory(navigation)
            | BreadcrumbPopover::ReanchoringDirectory { navigation, .. } => Some(navigation),
            _ => None,
        }
    }

    pub(crate) fn symbol_navigation(&self) -> Option<&BreadcrumbSymbolNavigation> {
        match &self.popover {
            BreadcrumbPopover::Symbol(navigation)
            | BreadcrumbPopover::ReanchoringSymbol { navigation, .. } => Some(navigation),
            _ => None,
        }
    }

    pub(crate) fn directory_popover_handle(&self) -> Option<Rc<dyn ErasedBreadcrumbPopoverHandle>> {
        self.directory_popover_handle.clone()
    }

    pub(crate) fn symbol_popover_handle(&self) -> Option<Rc<dyn ErasedBreadcrumbPopoverHandle>> {
        self.symbol_popover_handle.clone()
    }

    pub(crate) fn session_open(&self) -> bool {
        !matches!(self.popover, BreadcrumbPopover::Closed)
    }

    pub(crate) fn reanchoring(&self) -> bool {
        matches!(
            self.popover,
            BreadcrumbPopover::ReanchoringDirectory { .. }
                | BreadcrumbPopover::ReanchoringSymbol { .. }
        )
    }

    pub(crate) fn reanchor_pending(&self) -> bool {
        matches!(
            self.popover,
            BreadcrumbPopover::ReanchoringDirectory { pending: true, .. }
                | BreadcrumbPopover::ReanchoringSymbol { pending: true, .. }
        )
    }

    pub(crate) fn hide_popovers(&self, cx: &mut App) {
        if let Some(handle) = &self.directory_popover_handle {
            handle.hide(cx);
        }
        if let Some(handle) = &self.symbol_popover_handle {
            handle.hide(cx);
        }
    }

    /// Keeps an in-flight reanchor open: its `show` reopens the dropdown through this same session.
    pub(crate) fn set_directory_navigation(&mut self, navigation: BreadcrumbNavigation) {
        self.popover = if self.reanchoring() {
            BreadcrumbPopover::ReanchoringDirectory {
                navigation,
                pending: false,
            }
        } else {
            BreadcrumbPopover::Directory(navigation)
        };
    }

    pub(crate) fn set_symbol_navigation(&mut self, navigation: BreadcrumbSymbolNavigation) {
        self.popover = if self.reanchoring() {
            BreadcrumbPopover::ReanchoringSymbol {
                navigation,
                pending: false,
            }
        } else {
            BreadcrumbPopover::Symbol(navigation)
        };
    }

    pub(crate) fn begin_directory_reanchor(&mut self, navigation: BreadcrumbNavigation) {
        self.popover = BreadcrumbPopover::ReanchoringDirectory {
            navigation,
            pending: false,
        };
    }

    pub(crate) fn begin_symbol_reanchor(&mut self, navigation: BreadcrumbSymbolNavigation) {
        self.popover = BreadcrumbPopover::ReanchoringSymbol {
            navigation,
            pending: false,
        };
    }

    pub(crate) fn queue_reanchor(&mut self) -> bool {
        match &mut self.popover {
            BreadcrumbPopover::ReanchoringDirectory { pending, .. }
            | BreadcrumbPopover::ReanchoringSymbol { pending, .. } => {
                *pending = true;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn take_queued_reanchor(&mut self) -> bool {
        match &mut self.popover {
            BreadcrumbPopover::ReanchoringDirectory { pending, .. }
            | BreadcrumbPopover::ReanchoringSymbol { pending, .. }
                if *pending =>
            {
                *pending = false;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn reanchoring_popover_handle(
        &self,
    ) -> Option<Rc<dyn ErasedBreadcrumbPopoverHandle>> {
        match &self.popover {
            BreadcrumbPopover::ReanchoringDirectory { .. } => self.directory_popover_handle.clone(),
            BreadcrumbPopover::ReanchoringSymbol { .. } => self.symbol_popover_handle.clone(),
            _ => None,
        }
    }

    pub(crate) fn finish_reanchor(&mut self) {
        self.popover = match std::mem::replace(&mut self.popover, BreadcrumbPopover::Closed) {
            BreadcrumbPopover::ReanchoringDirectory { navigation, .. } => {
                BreadcrumbPopover::Directory(navigation)
            }
            BreadcrumbPopover::ReanchoringSymbol { navigation, .. } => {
                BreadcrumbPopover::Symbol(navigation)
            }
            other => other,
        };
    }

    /// A no-op while reanchoring: the dropdown is only moving between segments, not closing.
    pub(crate) fn clear_directory_navigation(
        &mut self,
        worktree_id: WorktreeId,
        path: &Arc<RelPath>,
    ) -> bool {
        match &self.popover {
            BreadcrumbPopover::Directory(navigation)
                if navigation.worktree_id == worktree_id && &navigation.active_path == path =>
            {
                self.close_session();
                true
            }
            _ => false,
        }
    }

    pub(crate) fn clear_symbol_navigation(
        &mut self,
        buffer_id: BufferId,
        active_item: Option<&OutlineItem<Anchor>>,
    ) -> bool {
        match &self.popover {
            BreadcrumbPopover::Symbol(navigation)
                if navigation.buffer_id == buffer_id
                    && navigation.active_item.as_ref().map(|item| &item.range)
                        == active_item.map(|item| &item.range) =>
            {
                self.close_session();
                true
            }
            _ => false,
        }
    }

    fn close_session(&mut self) {
        self.popover = BreadcrumbPopover::Closed;
        if !self.zone_mouse_down {
            self.expanded = false;
        }
    }

    pub(crate) fn close(&mut self) -> bool {
        let changed = self.session_open() || self.expanded;
        self.popover = BreadcrumbPopover::Closed;
        self.expanded = false;
        self.dismiss_subscription = None;
        changed
    }

    fn cached_symbol_trail(
        &self,
        buffer_id: BufferId,
        version: &clock::Global,
        range: &Range<Anchor>,
    ) -> Option<Vec<OutlineItem<text::Anchor>>> {
        let cache = self.symbol_trail_cache.borrow();
        let cache = cache.as_ref()?;
        (cache.buffer_id == buffer_id && &cache.version == version && &cache.range == range)
            .then(|| cache.trail.clone())
    }

    fn cache_symbol_trail(
        &self,
        buffer_id: BufferId,
        version: clock::Global,
        range: Range<Anchor>,
        trail: &[OutlineItem<text::Anchor>],
    ) {
        *self.symbol_trail_cache.borrow_mut() = Some(SymbolTrailCache {
            buffer_id,
            version,
            range,
            trail: trail.to_vec(),
        });
    }
}

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
pub fn flatten_text_for_single_line_display(text: &SharedString) -> SharedString {
    if text.contains('\n') {
        text.replace('\n', " ").into()
    } else {
        text.clone()
    }
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
    /// The hard cap's "⋯" pseudo-segment: rendered as an expand trigger, like a layout ellipsis.
    hard_cap_ellipsis: bool,
}

/// Measured once per render; `shape_line` is cached by text and font. Widths are
/// per item: the separator between two of them is [`breadcrumb_layout_plan_width`]'s.
#[derive(Clone)]
pub(crate) struct BreadcrumbSegmentMetrics {
    pub(crate) widths: Vec<Pixels>,
    pub(crate) ellipsis_width: Pixels,
    pub(crate) separator_width: Pixels,
}

impl BreadcrumbSegmentMetrics {
    fn natural_width(&self) -> Pixels {
        let separators = self.separator_width * self.widths.len().saturating_sub(1) as f32;
        self.widths
            .iter()
            .fold(Pixels::ZERO, |total, width| total + *width)
            + separators
    }
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
    /// An excerpt header row: its ellipsis must render as a plain marker, since it can never expand.
    multibuffer_header: bool,
    #[cfg(test)]
    probe: Option<Rc<BreadcrumbRowProbe>>,
}

/// Layout facts the painted scene keeps from a test: children past the row's bounds, and clamps.
#[cfg(test)]
#[derive(Default)]
struct BreadcrumbRowProbe {
    painted_extent: std::cell::Cell<Pixels>,
    last_segment_max_width: std::cell::Cell<Option<Pixels>>,
    dropped_runs: std::cell::Cell<usize>,
    bounds_width: std::cell::Cell<Pixels>,
}

const BREADCRUMB_SEGMENT_GROUP: &str = "breadcrumb-segment";

const BREADCRUMB_LABEL_PADDING: Pixels = px(4.);

const BREADCRUMB_ICON_SIZE: IconSize = IconSize::Small;

/// Taffy sizes every painted box on the device grid; a logical ceiling over-reserves at fractional scales.
fn ceil_to_device_pixel(width: Pixels, scale_factor: f32) -> Pixels {
    px(width.scale(scale_factor).ceil().as_f32() / scale_factor)
}

impl BreadcrumbsRow {
    fn measure(&self, window: &mut Window) -> BreadcrumbSegmentMetrics {
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let gap = window.rem_size() * 0.25;
        let scale_factor = window.scale_factor();

        let arrow_width = IconSize::XSmall.rems().to_pixels(window.rem_size());

        let ellipsis_run = text_style.to_run("⋯".len());
        // `TextLayout` rounds a shaped line up to a whole logical pixel before taffy sizes its box.
        let ellipsis_label_width = window
            .text_system()
            .shape_line("⋯".into(), font_size, &[ellipsis_run], None)
            .width()
            .ceil();
        let ellipsis_width = ceil_to_device_pixel(
            ellipsis_label_width + BREADCRUMB_LABEL_PADDING * 2.,
            scale_factor,
        );

        let widths = self
            .segments
            .iter()
            .map(|segment| {
                let text = flatten_text_for_single_line_display(&segment.label.text);
                let runs = segment_text_runs(segment, &text, &text_style);
                let label_width = window
                    .text_system()
                    .shape_line(text, font_size, &runs, None)
                    .width()
                    .ceil();
                let icon_width = if segment.icon.is_some() {
                    BREADCRUMB_ICON_SIZE.rems().to_pixels(window.rem_size()) + gap
                } else {
                    Pixels::ZERO
                };
                ceil_to_device_pixel(
                    icon_width
                        + label_width
                        + BREADCRUMB_LABEL_PADDING * 2.
                        + self.segment_padding(segment),
                    scale_factor,
                )
            })
            .collect();

        BreadcrumbSegmentMetrics {
            widths,
            ellipsis_width,
            separator_width: ceil_to_device_pixel(arrow_width + gap * 2., scale_factor),
        }
    }

    /// What the picker's trigger wraps an interactive segment in; zero for a plain label.
    fn segment_padding(&self, segment: &PreparedBreadcrumbSegment) -> Pixels {
        if segment.hard_cap_ellipsis || segment.target.is_none() || self.editor.is_none() {
            return Pixels::ZERO;
        }
        BREADCRUMB_PICKER_RENDERERS
            .get()
            .map_or(Pixels::ZERO, |renderers| renderers.segment_padding)
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

    fn render_segment(
        &self,
        index: usize,
        position: usize,
        last_position: usize,
        max_width: Option<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let segment = &self.segments[index];
        if segment.hard_cap_ellipsis {
            return self.render_ellipsis(position, last_position, cx);
        }
        let mut text_style = window.text_style();
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
        let text = if let Some(max_width) = max_width {
            let gap = window.rem_size() * 0.25;
            let icon_width = if segment.icon.is_some() {
                BREADCRUMB_ICON_SIZE.rems().to_pixels(window.rem_size()) + gap
            } else {
                Pixels::ZERO
            };
            let label_width = (max_width
                - BREADCRUMB_LABEL_PADDING * 2.
                - icon_width
                - self.segment_padding(segment))
            .max(Pixels::ZERO);
            div()
                .max_w(label_width)
                .truncate()
                .child(text)
                .into_any_element()
        } else {
            text
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
                    .id(("breadcrumb-segment", position))
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
        wrap_segment(element)
    }

    fn render_ellipsis(&self, position: usize, last_position: usize, cx: &App) -> gpui::AnyElement {
        let content = Label::new("⋯").color(Color::Placeholder).into_any_element();
        // A header row can never expand, so only the main bar's ellipsis is live.
        let interactive = !self.multibuffer_header;
        let label = self.with_separator(position, last_position, content, interactive, cx);
        if !interactive {
            return label;
        }
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
            // An ellipsis has no path of its own; keep the bar's copy fallback from firing.
            .on_mouse_down(gpui::MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(label)
            .into_any_element();
        wrap_segment(element)
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

fn wrap_segment(element: gpui::AnyElement) -> gpui::AnyElement {
    div()
        .group(BREADCRUMB_SEGMENT_GROUP)
        .child(element)
        .into_any_element()
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

/// The trail's last segment, the one collapsing never drops: the deepest symbol at the
/// cursor, or the file segment when the buffer contributes none. Reads the same
/// `outline_symbols_at_cursor` that [`render_breadcrumb_text`] turns into symbol segments.
pub(crate) fn breadcrumb_leaf_navigation(
    editor: &Editor,
    cx: &App,
) -> Option<BreadcrumbSymbolNavigation> {
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let buffer_id = buffer.read(cx).remote_id();
    let active_item = editor
        .outline_symbols_at_cursor
        .as_ref()
        .filter(|(id, _)| *id == buffer_id)
        .and_then(|(_, ancestors)| ancestors.last().cloned());
    Some(BreadcrumbSymbolNavigation {
        buffer_id,
        active_item,
        navigated: false,
    })
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
        let natural_width = metrics.natural_width();
        let line_height = window.text_style().line_height_in_pixels(window.rem_size());

        let measured = metrics.clone();
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
                                    &measured,
                                    &kinds,
                                    available_width,
                                    anchored_index,
                                    file_outlives_symbols,
                                );
                                // Even the minimal plan can overflow; prepaint then truncates the last label.
                                breadcrumb_layout_plan_width(&measured, &plan).min(available_width)
                            }
                            AvailableSpace::MinContent => measured
                                .widths
                                .last()
                                .copied()
                                .unwrap_or(measured.ellipsis_width)
                                .max(measured.ellipsis_width),
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
        let plan = if self.expanded {
            BreadcrumbLayoutPlan {
                visible: (0..kinds.len()).collect(),
                ellipses: Vec::new(),
            }
        } else {
            plan_breadcrumb_layout(
                metrics,
                &kinds,
                bounds.size.width,
                anchored_index,
                self.file_outlives_symbols,
            )
        };

        if let Some(anchored_index) = anchored_index
            && !plan.visible.contains(&anchored_index)
            && let Some(target) = self
                .segments
                .get(anchored_index)
                .and_then(|segment| segment.target.clone())
            && let Some(editor) = self.editor.as_ref().and_then(WeakEntity::upgrade)
            // Reanchoring drops and re-shows the popover itself; dismissing mid-flight fights it.
            && !editor.read(cx).breadcrumb_reanchoring()
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
        // Nothing left to drop and still too wide: the last label must ellipsize itself.
        let truncate_last =
            !self.expanded && breadcrumb_layout_plan_width(metrics, &plan) > bounds.size.width;
        let gap = window.rem_size() * 0.25;
        let mut x = bounds.origin.x;
        let mut children = Vec::with_capacity(sequence.len());
        for (position, item) in sequence.into_iter().enumerate() {
            let max_width = (truncate_last && position == last_position)
                .then(|| (bounds.origin.x + bounds.size.width - x).max(Pixels::ZERO));
            #[cfg(test)]
            if position == last_position
                && let Some(probe) = &self.probe
            {
                probe.last_segment_max_width.set(max_width);
            }
            let mut element = match item {
                FinalItem::Segment(index) => {
                    self.render_segment(index, position, last_position, max_width, window, cx)
                }
                FinalItem::Ellipsis => self.render_ellipsis(position, last_position, cx),
            };
            let available_space = size(
                AvailableSpace::MaxContent,
                AvailableSpace::Definite(bounds.size.height),
            );
            let element_size = element.layout_as_root(available_space, window, cx);
            // The 22px triggers outgrow a line-height row; centering keeps them inside the bar.
            let y = bounds.origin.y + (bounds.size.height - element_size.height) / 2.;
            element.prepaint_at(point(x, y), window, cx);
            x += element_size.width + gap;
            children.push(element);
        }

        #[cfg(test)]
        if let Some(probe) = &self.probe {
            probe
                .painted_extent
                .set((x - gap - bounds.origin.x).max(Pixels::ZERO));
            probe.dropped_runs.set(plan.ellipses.len());
            probe.bounds_width.set(bounds.size.width);
        }

        if let Some(editor) = self.editor.as_ref().and_then(WeakEntity::upgrade)
            && editor.read(cx).breadcrumb_reanchor_pending()
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
    window: &mut Window,
    cx: &mut App,
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
    let mut file_icon_color = Color::Muted;

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
            // The prefix icon resolves its own tint; skip the second lookup per frame.
            if prefix.is_none() && listing_settings.file_icons {
                file_icon_color =
                    bar_file_icon_color(editor_ref.project(), real_project_path.as_ref(), cx);
            }
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

            // A single-file worktree (a file opened outside any real worktree) has no tree to browse.
            let is_navigable = real_project_path.as_ref().is_some_and(|project_path| {
                !editor_ref
                    .project()
                    .and_then(|project| {
                        project
                            .read(cx)
                            .worktree_for_id(project_path.worktree_id, cx)
                    })
                    .is_some_and(|worktree| worktree.read(cx).is_single_file())
            });

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
                // Even a non-navigable path gets a target: the file segment still opens the outline picker.
                symbol_segments.push(Some(BreadcrumbSegmentTarget::Symbol {
                    buffer_id,
                    item: None,
                    is_active_segment: file_segment_active,
                }));
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
                        .map(|item| symbol_trail_with_fallback(editor_ref, buffer_id, item, cx))
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

    // A multibuffer excerpt header has no scroll container, so its row never expands.
    let expanded = !multibuffer_header
        && editor
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|editor_entity| editor_entity.read(cx).breadcrumb_expanded());

    let symbol_segments = align_symbol_segments(&segments, symbol_segments);
    let kinds =
        classify_breadcrumb_segment_kinds(segments.len(), file_segment_index, has_root_segment);
    let (segments, symbol_segments, kinds, file_segment_index, hard_cap_index) =
        hard_cap_breadcrumb_middle_segments(
            segments,
            symbol_segments,
            kinds,
            file_segment_index,
            expanded,
        );

    // At most one file icon on screen: the tab-bar-hidden prefix already renders one.
    let file_icon =
        if prefix.is_some() || !BreadcrumbDirectoryListingSettings::get_global(cx).file_icons {
            None
        } else {
            file_path_for_icon
                .as_deref()
                .and_then(|path| file_icons::FileIcons::get_icon(path.as_std_path(), cx))
        };
    let file_status_color = crate::element::file_status_label_color(file_status);

    let tab_bar_hidden = !workspace::TabBarSettings::get_global(cx).show;
    let apply_dirty_filename_style = tab_bar_hidden && active_item.is_dirty(cx);

    let prepared_segments = segments
        .into_iter()
        .zip(symbol_segments)
        .zip(kinds)
        .enumerate()
        .map(|(index, ((label, target), kind))| {
            // A navigated bar can put a Directory at file_segment_index; only the bare file target counts.
            let is_file_segment = kind == BreadcrumbSegmentKind::File
                && matches!(
                    target.as_ref(),
                    Some(BreadcrumbSegmentTarget::Symbol { item: None, .. })
                );
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
                hard_cap_ellipsis: Some(index) == hard_cap_index,
            }
        })
        .collect();

    let row = BreadcrumbsRow {
        segments: prepared_segments,
        editor: editor.clone(),
        expanded,
        file_outlives_symbols: tab_bar_hidden,
        multibuffer_header,
        #[cfg(test)]
        probe: None,
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
            .min_w_0()
            .child(row)
            // Thin and bottom-hugging, so the thumb stays below the crumb glyphs.
            .custom_scrollbars(
                Scrollbars::new(ScrollAxes::Horizontal).thumb_geometry(px(4.), px(1.)),
                window,
                cx,
            )
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
            // Capture phase: runs before `PopoverMenu`'s bubbled dismiss for the same click.
            .capture_any_mouse_down({
                let editor = editor.clone();
                move |_, window, cx| {
                    if let Some(editor) = editor.upgrade() {
                        editor.update(cx, |editor, cx| {
                            editor.note_breadcrumb_zone_mouse_down(window, cx)
                        });
                    }
                }
            })
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
            .h(rems_from_px(22.)) // Match the height and padding of the h_flex in the other arm.
            .pl_1()
            .child(breadcrumbs)
            .into_any_element(),
    }
}

/// An edit can rewrite the drilled symbol's region; the stored item keeps the anchored segment alive until a fresh outline lands.
///
/// Reached on every bar render while a symbol session is live, so the trail is
/// memoized against the stored outline's version, in buffer coordinates: the
/// multibuffer layout can change without touching the cache key.
fn symbol_trail_with_fallback(
    editor: &Editor,
    buffer_id: BufferId,
    item: &OutlineItem<Anchor>,
    cx: &App,
) -> Vec<OutlineItem<Anchor>> {
    let version = editor.breadcrumb_outline_version(buffer_id);
    let buffer_trail = match version.and_then(|version| {
        editor
            .breadcrumb_state
            .cached_symbol_trail(buffer_id, version, &item.range)
    }) {
        Some(trail) => trail,
        None => {
            let trail = editor.breadcrumb_symbol_trail_in_buffer(buffer_id, item, cx);
            if let Some(version) = version {
                editor.breadcrumb_state.cache_symbol_trail(
                    buffer_id,
                    version.clone(),
                    item.range.clone(),
                    &trail,
                );
            }
            trail
        }
    };
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    // All-or-nothing: a partially converted trail would render with a silent hole.
    let trail = buffer_trail
        .iter()
        .map(|trail_item| {
            crate::document_symbols::text_outline_item_to_multibuffer(trail_item, &snapshot)
        })
        .collect::<Option<Vec<_>>>();
    match trail {
        Some(trail) if !trail.is_empty() => trail,
        _ => vec![item.clone()],
    }
}

// Deliberately not gated on `diagnostic_badges`: that setting only affects picker rows.
pub(crate) fn bar_file_icon_color(
    project: Option<&Entity<project::Project>>,
    project_path: Option<&project::ProjectPath>,
    cx: &App,
) -> Color {
    let severity = project
        .zip(project_path)
        .and_then(|(project, project_path)| {
            path::breadcrumb_diagnostic_severity(
                project.read(cx),
                project_path,
                BreadcrumbDirectoryListingSettings::get_global(cx).show_diagnostics,
                cx,
            )
        });
    crate::items::entry_diagnostic_aware_icon_decoration_and_color(severity)
        .map(|(_, color)| color)
        .unwrap_or(Color::Muted)
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
    fn test_flatten_text_for_single_line_display_preserves_byte_offsets() {
        // Byte-offset ranges must locate the same substring in both strings.
        let original = SharedString::from("fn outer() {\n    inner()\n}");
        let flattened = flatten_text_for_single_line_display(&original);

        assert_eq!(flattened, "fn outer() {     inner() }");
        assert_eq!(flattened.len(), original.len());

        let inner_offset = original.find("inner").unwrap();
        assert_eq!(
            &flattened[inner_offset..inner_offset + "inner".len()],
            "inner",
        );
    }

    #[test]
    fn test_dirty_filename_styles_only_change_font_weight() {
        let mut base = gpui::TextStyle::default();
        base.color = gpui::red();

        let dirty = dirty_filename_text_style(&base);
        assert_eq!(dirty.color, base.color, "the git status color must survive");
        assert_eq!(dirty.font_weight, FontWeight::BOLD);

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

    #[gpui::test]
    fn test_symbol_trail_survives_an_outline_that_lost_the_drilled_range(
        cx: &mut gpui::TestAppContext,
    ) {
        crate::editor_tests::init_test(cx, |_| {});

        let buffer = cx.new(|cx| language::Buffer::local("fn alpha() {}\nfn beta() {}\n", cx));
        let multi_buffer = cx.new(|cx| multi_buffer::MultiBuffer::singleton(buffer.clone(), cx));
        let editor_window =
            cx.add_window(|window, cx| crate::test::build_editor(multi_buffer, window, cx));
        let editor = editor_window.root(cx).unwrap();
        let buffer_id = buffer.read_with(cx, |buffer, _| buffer.remote_id());

        let (version, item_alpha, item_beta) = buffer.read_with(cx, |buffer, _| {
            let snapshot = buffer.snapshot();
            let make = |start: usize, end: usize, text: &str| OutlineItem {
                depth: 0,
                range: snapshot.anchor_before(start)..snapshot.anchor_after(end),
                selection_range: snapshot.anchor_before(start)..snapshot.anchor_after(end),
                source_range_for_text: snapshot.anchor_before(start)..snapshot.anchor_after(end),
                text: text.into(),
                highlight_ranges: Vec::new(),
                name_ranges: Vec::new(),
                body_range: None,
                annotation_range: None,
            };
            (
                buffer.version(),
                make(0, 13, "fn alpha"),
                make(14, 26, "fn beta"),
            )
        });

        editor.update(cx, |editor, cx| {
            editor.set_breadcrumb_outline(buffer_id, version.clone(), vec![item_alpha], cx);
        });
        let stored = editor.read_with(cx, |editor, cx| {
            editor
                .breadcrumb_symbol_menu_items(buffer_id, None, cx)
                .first()
                .cloned()
                .expect("the fresh outline lists its one symbol")
        });
        editor.read_with(cx, |editor, cx| {
            assert_eq!(
                editor.breadcrumb_symbol_trail(buffer_id, &stored, cx).len(),
                1,
                "sanity: the fresh outline resolves the trail"
            );
        });

        editor.update(cx, |editor, cx| {
            editor.set_breadcrumb_outline(buffer_id, version, vec![item_beta], cx);
        });
        editor.read_with(cx, |editor, cx| {
            assert!(
                editor
                    .breadcrumb_symbol_trail(buffer_id, &stored, cx)
                    .is_empty(),
                "sanity: the replaced outline no longer contains the drilled range"
            );
            let trail = symbol_trail_with_fallback(editor, buffer_id, &stored, cx);
            assert_eq!(
                trail
                    .iter()
                    .map(|item| item.text.clone())
                    .collect::<Vec<_>>(),
                vec![stored.text.clone()],
                "the stored segment must survive a lookup miss instead of vanishing"
            );
        });
    }

    /// Moving the buffer to a new path key rebuilds its excerpts without
    /// touching the buffer version, which is the cache key.
    #[gpui::test]
    fn test_cached_symbol_trail_follows_multibuffer_layout_changes(cx: &mut gpui::TestAppContext) {
        crate::editor_tests::init_test(cx, |_| {});

        let buffer = cx.new(|cx| language::Buffer::local("fn alpha() {}\n", cx));
        let excerpt_ranges = vec![multi_buffer::ExcerptRange::new(
            language::Point::new(0, 0)..language::Point::new(0, 13),
        )];
        let multi_buffer = cx.new(|cx| {
            let mut multi_buffer = multi_buffer::MultiBuffer::new(language::Capability::ReadWrite);
            multi_buffer.set_excerpt_ranges_for_path(
                multi_buffer::PathKey::sorted(0),
                buffer.clone(),
                &buffer.read(cx).snapshot(),
                excerpt_ranges.clone(),
                cx,
            );
            multi_buffer
        });
        let editor_window =
            cx.add_window(|window, cx| crate::test::build_editor(multi_buffer.clone(), window, cx));
        let editor = editor_window.root(cx).unwrap();
        let buffer_id = buffer.read_with(cx, |buffer, _| buffer.remote_id());

        let (version, item) = buffer.read_with(cx, |buffer, _| {
            let snapshot = buffer.snapshot();
            let item = OutlineItem {
                depth: 0,
                range: snapshot.anchor_before(0)..snapshot.anchor_after(13),
                selection_range: snapshot.anchor_before(0)..snapshot.anchor_after(13),
                source_range_for_text: snapshot.anchor_before(0)..snapshot.anchor_after(13),
                text: "fn alpha".into(),
                highlight_ranges: Vec::new(),
                name_ranges: Vec::new(),
                body_range: None,
                annotation_range: None,
            };
            (buffer.version(), item)
        });
        editor.update(cx, |editor, cx| {
            editor.set_breadcrumb_outline(buffer_id, version, vec![item], cx);
        });
        let stored = editor.read_with(cx, |editor, cx| {
            editor
                .breadcrumb_symbol_menu_items(buffer_id, None, cx)
                .first()
                .cloned()
                .expect("the fresh outline lists its one symbol")
        });
        editor.read_with(cx, |editor, cx| {
            let trail = symbol_trail_with_fallback(editor, buffer_id, &stored, cx);
            assert_eq!(trail.len(), 1, "sanity: the first read populates the cache");
        });

        multi_buffer.update(cx, |multi_buffer, cx| {
            multi_buffer.set_excerpt_ranges_for_path(
                multi_buffer::PathKey::sorted(1),
                buffer.clone(),
                &buffer.read(cx).snapshot(),
                excerpt_ranges,
                cx,
            );
        });

        editor.read_with(cx, |editor, cx| {
            let expected = editor
                .breadcrumb_symbol_menu_items(buffer_id, None, cx)
                .first()
                .cloned()
                .expect("the moved excerpts still list the symbol");
            let trail = symbol_trail_with_fallback(editor, buffer_id, &stored, cx);
            assert_eq!(
                trail.last().map(|item| item.range.clone()),
                Some(expected.range),
                "a cached trail must be re-anchored into the current multibuffer layout"
            );
        });
    }

    struct DismissEmitter;

    impl gpui::EventEmitter<gpui::DismissEvent> for DismissEmitter {}

    #[gpui::test]
    fn test_breadcrumb_zone_mouse_down_keeps_trail_expanded_through_dismiss(
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
        let picker = cx.new(|_| DismissEmitter);

        // Production ordering: the bar's capture handler notes the mouse-down,
        // then `PopoverMenu` emits a dismiss from the same click; the emitted
        // event reaches the subscription only after this update returns.
        editor_window
            .update(cx, |editor, window, cx| {
                editor.expand_breadcrumb_trail(cx);
                editor.open_breadcrumb_navigation(worktree_id, path.clone(), cx);
                editor.watch_breadcrumb_dismissal(&picker, worktree_id, path.clone(), cx);
                editor.note_breadcrumb_zone_mouse_down(window, cx);
                picker.update(cx, |_, cx| cx.emit(gpui::DismissEvent));
            })
            .unwrap();
        cx.run_until_parked();
        editor.read_with(cx, |editor, _| {
            assert!(
                editor.breadcrumb_navigation().is_none(),
                "the dismiss must still close the session"
            );
            assert!(
                editor.breadcrumb_expanded(),
                "a dismiss caused by a click on the bar must not collapse the trail"
            );
        });

        cx.update_window(editor_window.into(), |_, window, cx| {
            window.simulate_next_frame(cx)
        })
        .unwrap();
        editor.read_with(cx, |editor, _| {
            assert!(
                !editor.breadcrumb_zone_mouse_down(),
                "the next frame must clear the one-frame flag"
            );
        });

        // Same dismiss with no preceding mouse-down on the bar: collapses.
        editor_window
            .update(cx, |editor, _, cx| {
                editor.open_breadcrumb_navigation(worktree_id, path.clone(), cx);
                editor.watch_breadcrumb_dismissal(&picker, worktree_id, path.clone(), cx);
                picker.update(cx, |_, cx| cx.emit(gpui::DismissEvent));
            })
            .unwrap();
        cx.run_until_parked();
        editor.read_with(cx, |editor, _| {
            assert!(
                !editor.breadcrumb_expanded(),
                "a dismiss from outside the bar still collapses the trail"
            );
        });
    }

    #[gpui::test]
    async fn test_bar_file_icon_tint_does_not_require_diagnostic_badges(
        cx: &mut gpui::TestAppContext,
    ) {
        use language::{Diagnostic, DiagnosticEntry, DiagnosticSourceKind};
        use lsp::{DiagnosticSeverity as LspDiagnosticSeverity, LanguageServerId};
        use project::{FakeFs, Project, ProjectPath};
        use serde_json::json;
        use std::path::Path;
        use text::{PointUtf16, Unclipped};
        use util::path;

        crate::editor_tests::init_test(cx, |_| {});

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/root"), json!({ "error.txt": "" }))
            .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let worktree_id = project.update(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });
        cx.run_until_parked();

        let lsp_store = project.read_with(cx, |project, _| project.lsp_store());
        lsp_store.update(cx, |lsp_store, cx| {
            lsp_store
                .update_diagnostic_entries(
                    LanguageServerId(0),
                    Path::new(path!("/root/error.txt")).to_owned(),
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

        let project_path = ProjectPath {
            worktree_id,
            path: util::rel_path::rel_path("error.txt").into_arc(),
        };
        let color = cx.update(|cx| bar_file_icon_color(Some(&project), Some(&project_path), cx));
        assert_eq!(
            color,
            Color::Error,
            "the bar tint must not depend on the picker-only diagnostic_badges setting"
        );
    }

    #[gpui::test]
    async fn test_leading_file_icon_inherits_diagnostic_tint_when_tab_bar_hidden(
        cx: &mut gpui::TestAppContext,
    ) {
        use language::{Diagnostic, DiagnosticEntry, DiagnosticSourceKind};
        use lsp::{DiagnosticSeverity as LspDiagnosticSeverity, LanguageServerId};
        use project::{FakeFs, Project};
        use serde_json::json;
        use settings::SettingsStore;
        use std::path::Path;
        use text::{PointUtf16, Unclipped};
        use util::path;

        crate::editor_tests::init_test(cx, |_| {});
        cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|settings, cx| {
                settings.update_user_settings(cx, |settings| {
                    settings.tab_bar.get_or_insert_default().show = Some(false);
                    settings.tabs.get_or_insert_default().file_icons = Some(true);
                });
            });
        });

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/root"), json!({ "error.txt": "boom" }))
            .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        cx.run_until_parked();

        let lsp_store = project.read_with(cx, |project, _| project.lsp_store());
        lsp_store.update(cx, |lsp_store, cx| {
            lsp_store
                .update_diagnostic_entries(
                    LanguageServerId(0),
                    Path::new(path!("/root/error.txt")).to_owned(),
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

        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/root/error.txt"), cx)
            })
            .await
            .unwrap();
        let multi_buffer = cx.new(|cx| multi_buffer::MultiBuffer::singleton(buffer, cx));
        let editor_window = cx.add_window(|window, cx| {
            crate::test::build_editor_with_project(project.clone(), multi_buffer, window, cx)
        });
        let editor = editor_window.root(cx).unwrap();

        let icon = editor.read_with(cx, |editor, cx| editor.breadcrumb_prefix_icon(cx));
        let (_, color) = icon.expect("a hidden tab bar must yield a leading icon");
        assert_eq!(
            color,
            Color::Error,
            "the leading icon must carry the same diagnostic tint as the file segment"
        );
    }

    fn draw_breadcrumb_row(
        labels: Vec<SharedString>,
        expanded: bool,
        cx: &mut gpui::TestAppContext,
    ) -> (Pixels, Pixels) {
        let (scroll_range, probe) =
            draw_breadcrumb_row_in_container(labels, expanded, px(200.), None, false, cx);
        (scroll_range, probe.painted_extent.get())
    }

    fn draw_breadcrumb_row_in_container(
        labels: Vec<SharedString>,
        expanded: bool,
        container_width: Pixels,
        anchored_index: Option<usize>,
        last_segment_icon: bool,
        cx: &mut gpui::TestAppContext,
    ) -> (Pixels, Rc<BreadcrumbRowProbe>) {
        crate::editor_tests::init_test(cx, |_| {});
        draw_breadcrumb_row_probe(
            labels,
            expanded,
            container_width,
            anchored_index,
            last_segment_icon,
            None,
            cx,
        )
    }

    fn draw_breadcrumb_row_probe(
        labels: Vec<SharedString>,
        expanded: bool,
        container_width: Pixels,
        anchored_index: Option<usize>,
        last_segment_icon: bool,
        editor: Option<WeakEntity<Editor>>,
        cx: &mut gpui::TestAppContext,
    ) -> (Pixels, Rc<BreadcrumbRowProbe>) {
        let scroll_handle = gpui::ScrollHandle::new();
        let probe = Rc::new(BreadcrumbRowProbe::default());
        let window = cx.add_window({
            let scroll_handle = scroll_handle.clone();
            let probe = probe.clone();
            move |_, _| ScrollProbe {
                labels,
                expanded,
                container_width,
                anchored_index,
                last_segment_icon,
                editor,
                scroll_handle,
                probe,
            }
        });
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
        (scroll_handle.max_offset().x, probe)
    }

    fn breadcrumb_row_scroll_range(expanded: bool, cx: &mut gpui::TestAppContext) -> Pixels {
        let labels = (0..12)
            .map(|index| SharedString::from(format!("directory-with-a-long-name-{index}")))
            .collect();
        draw_breadcrumb_row(labels, expanded, cx).0
    }

    struct ScrollProbe {
        labels: Vec<SharedString>,
        expanded: bool,
        container_width: Pixels,
        anchored_index: Option<usize>,
        last_segment_icon: bool,
        editor: Option<WeakEntity<Editor>>,
        scroll_handle: gpui::ScrollHandle,
        probe: Rc<BreadcrumbRowProbe>,
    }

    impl Render for ScrollProbe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let last_index = self.labels.len().saturating_sub(1);
            let segments = self
                .labels
                .iter()
                .enumerate()
                .map(|(index, label)| PreparedBreadcrumbSegment {
                    kind: if self.last_segment_icon && index == last_index {
                        BreadcrumbSegmentKind::File
                    } else {
                        BreadcrumbSegmentKind::Middle
                    },
                    label: HighlightedText {
                        text: label.clone(),
                        highlights: Vec::new(),
                    },
                    target: (self.editor.is_some() || self.anchored_index == Some(index)).then(
                        || BreadcrumbSegmentTarget::Symbol {
                            buffer_id: BufferId::new(1).unwrap(),
                            item: None,
                            is_active_segment: self.anchored_index == Some(index),
                        },
                    ),
                    dirty_filename_style: false,
                    icon: (self.last_segment_icon && index == last_index)
                        .then(|| SharedString::from("icons/file_icons/file.svg")),
                    icon_color: Color::Muted,
                    label_color: Color::Muted,
                    hard_cap_ellipsis: false,
                })
                .collect();
            let row = BreadcrumbsRow {
                segments,
                editor: self.editor.clone(),
                expanded: self.expanded,
                file_outlives_symbols: false,
                multibuffer_header: false,
                probe: Some(self.probe.clone()),
            };
            // Mirrors the real chain, clipping box and nested rows included.
            h_flex().w(self.container_width).overflow_x_hidden().child(
                h_flex().flex_grow_1().min_w_0().child(
                    h_flex().min_w_0().child(
                        h_flex()
                            .min_w_0()
                            .child(row)
                            .custom_scrollbars(
                                Scrollbars::new(ScrollAxes::Horizontal)
                                    .thumb_geometry(px(4.), px(1.))
                                    .tracked_scroll_handle(&self.scroll_handle),
                                window,
                                cx,
                            )
                            // A tracked handle skips the automatic wiring an untracked one gets.
                            .track_scroll(&self.scroll_handle)
                            .overflow_x_scroll(),
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

    #[gpui::test]
    fn test_collapsed_row_truncates_an_oversized_last_segment(cx: &mut gpui::TestAppContext) {
        let label = SharedString::from(
            "a-single-segment-name-far-wider-than-the-two-hundred-pixel-container-it-lives-in",
        );

        let (_, natural_extent) = draw_breadcrumb_row(vec![label.clone()], true, cx);
        assert!(
            natural_extent > px(200.),
            "sanity: untruncated, the label must overflow the container, got {natural_extent:?}"
        );

        let (scroll_range, painted_extent) = draw_breadcrumb_row(vec![label], false, cx);
        assert_eq!(
            scroll_range,
            px(0.),
            "a collapsed row must fit its container instead of scrolling"
        );
        assert!(
            painted_extent <= px(200.),
            "the last segment must ellipsize instead of painting past the container, got {painted_extent:?}"
        );
    }

    #[gpui::test]
    fn test_anchored_middle_segment_keeps_the_last_label_inside_the_row(
        cx: &mut gpui::TestAppContext,
    ) {
        let middle =
            SharedString::from("an-anchored-middle-segment-that-eats-nearly-the-whole-row");

        let (_, probe) =
            draw_breadcrumb_row_in_container(vec![middle.clone()], true, px(200.), None, false, cx);
        // Leaves the last segment far less room than its natural width.
        let container = probe.painted_extent.get() + px(30.);
        let (_, probe) = draw_breadcrumb_row_in_container(
            vec![middle, SharedString::from("file-name")],
            false,
            container,
            Some(0),
            false,
            cx,
        );
        let painted_extent = probe.painted_extent.get();
        assert!(
            painted_extent <= container,
            "the truncated last label must stay inside the row, got {painted_extent:?} in {container:?}"
        );
    }

    /// The test platform pins every window to scale 2, so the fractional grid is only reachable here.
    #[test]
    fn test_ceil_to_device_pixel_reserves_on_the_device_grid() {
        assert_eq!(ceil_to_device_pixel(px(46.7), 1.), px(47.));
        assert_eq!(ceil_to_device_pixel(px(46.7), 1.5), px(71. / 1.5));
        assert!(ceil_to_device_pixel(px(46.7), 1.5) > px(46.7));
        assert_eq!(ceil_to_device_pixel(px(46.), 1.5), px(46.));
    }

    fn sample_trail_labels() -> Vec<SharedString> {
        [
            "ihavenever",
            "src",
            "main",
            "kotlin",
            "com",
            "kelstar",
            "ihne",
            "model",
            "Entities.kt",
        ]
        .into_iter()
        .map(SharedString::from)
        .collect()
    }

    #[gpui::test]
    fn test_a_row_that_fits_keeps_every_segment_and_clamps_none(cx: &mut gpui::TestAppContext) {
        let labels = sample_trail_labels();

        let (_, natural) =
            draw_breadcrumb_row_in_container(labels.clone(), true, px(4000.), None, true, cx);
        let natural_extent = natural.painted_extent.get();
        assert_eq!(
            natural.bounds_width.get(),
            natural_extent,
            "the row must reserve what it paints, or it collapses a trail that would have fit"
        );

        let (scroll_range, probe) =
            draw_breadcrumb_row_in_container(labels, false, natural_extent, None, true, cx);
        assert_eq!(scroll_range, px(0.));
        assert_eq!(
            probe.dropped_runs.get(),
            0,
            "a container the width of the trail must keep every segment"
        );
        assert_eq!(
            probe.last_segment_max_width.get(),
            None,
            "nothing overflows, so the last segment must not be clamped"
        );
        assert_eq!(probe.painted_extent.get(), natural_extent);
    }

    const PROBE_SEGMENT_PADDING: Pixels = px(2.);

    struct ProbePopoverHandle;

    impl ErasedBreadcrumbPopoverHandle for ProbePopoverHandle {
        fn hide(&self, _: &mut App) {}

        fn show(&self, _: &mut Window, _: &mut App) {}

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn probe_popover_handle() -> Rc<dyn ErasedBreadcrumbPopoverHandle> {
        Rc::new(ProbePopoverHandle)
    }

    /// Stands in for the picker's trigger: it paints exactly the padding the registration declares.
    fn pad_like_a_trigger(label: gpui::AnyElement) -> gpui::AnyElement {
        div()
            .px(PROBE_SEGMENT_PADDING / 2.)
            .child(label)
            .into_any_element()
    }

    fn probe_symbol_segment(
        _: WeakEntity<Editor>,
        _: BufferId,
        _: Option<OutlineItem<Anchor>>,
        _: Option<(WorktreeId, Arc<RelPath>)>,
        _: bool,
        _: Rc<dyn ErasedBreadcrumbPopoverHandle>,
        label: gpui::AnyElement,
        _: usize,
    ) -> gpui::AnyElement {
        pad_like_a_trigger(label)
    }

    fn probe_directory_segment(
        _: WeakEntity<Editor>,
        _: WeakEntity<Workspace>,
        _: WorktreeId,
        _: Arc<RelPath>,
        _: Option<Arc<RelPath>>,
        _: bool,
        _: Rc<dyn ErasedBreadcrumbPopoverHandle>,
        label: gpui::AnyElement,
        _: usize,
    ) -> gpui::AnyElement {
        pad_like_a_trigger(label)
    }

    fn register_probe_renderers() {
        BREADCRUMB_PICKER_RENDERERS.get_or_init(|| BreadcrumbPickerRenderers {
            directory: probe_directory_segment,
            symbol: probe_symbol_segment,
            popover_handle: probe_popover_handle,
            symbol_popover_handle: probe_popover_handle,
            segment_padding: PROBE_SEGMENT_PADDING,
        });
    }

    /// `breadcrumb_picker` pins the constant to its trigger; this pins the row to the constant.
    #[gpui::test]
    fn test_an_interactive_row_reserves_the_padding_its_triggers_paint(
        cx: &mut gpui::TestAppContext,
    ) {
        crate::editor_tests::init_test(cx, |_| {});
        register_probe_renderers();

        let buffer = cx.new(|cx| language::Buffer::local("fn main() {}", cx));
        let buffer = cx.new(|cx| multi_buffer::MultiBuffer::singleton(buffer, cx));
        let editor_window =
            cx.add_window(|window, cx| crate::test::build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap().downgrade();

        let (_, natural) = draw_breadcrumb_row_probe(
            sample_trail_labels(),
            true,
            px(4000.),
            None,
            true,
            Some(editor.clone()),
            cx,
        );
        let natural_extent = natural.painted_extent.get();
        assert_eq!(
            natural.bounds_width.get(),
            natural_extent,
            "the row must reserve the trigger padding it paints, not just the separators"
        );

        let (_, collapsed) = draw_breadcrumb_row_probe(
            sample_trail_labels(),
            false,
            natural_extent,
            None,
            true,
            Some(editor),
            cx,
        );
        assert_eq!(
            collapsed.dropped_runs.get(),
            0,
            "a container the width of the interactive trail must keep every segment"
        );
        assert_eq!(collapsed.painted_extent.get(), natural_extent);
    }

    struct EllipsisFallbackProbe {
        editor: WeakEntity<Editor>,
    }

    impl Render for EllipsisFallbackProbe {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let row = BreadcrumbsRow {
                segments: vec![PreparedBreadcrumbSegment {
                    kind: BreadcrumbSegmentKind::Middle,
                    label: HighlightedText {
                        text: "⋯".into(),
                        highlights: Vec::new(),
                    },
                    target: None,
                    dirty_filename_style: false,
                    icon: None,
                    icon_color: Color::Muted,
                    label_color: Color::Muted,
                    hard_cap_ellipsis: true,
                }],
                editor: Some(self.editor.clone()),
                expanded: false,
                file_outlives_symbols: false,
                multibuffer_header: false,
                probe: None,
            };
            // The bar-level fallback from `render_breadcrumb_text`, reduced to its clipboard write.
            div()
                .size_full()
                .on_mouse_down(gpui::MouseButton::Right, |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string("fallback".to_string()))
                })
                .child(div().w(px(60.)).h(px(22.)).child(row))
        }
    }

    #[gpui::test]
    fn test_right_click_on_ellipsis_does_not_reach_the_copy_fallback(
        cx: &mut gpui::TestAppContext,
    ) {
        crate::editor_tests::init_test(cx, |_| {});

        let buffer = cx.new(|cx| language::Buffer::local("fn main() {}", cx));
        let buffer = cx.new(|cx| multi_buffer::MultiBuffer::singleton(buffer, cx));
        let editor_window =
            cx.add_window(|window, cx| crate::test::build_editor(buffer, window, cx));
        let editor = editor_window.root(cx).unwrap().downgrade();

        let probe_window = cx.add_window(|_, _| EllipsisFallbackProbe { editor });
        let cx = &mut gpui::VisualTestContext::from_window(probe_window.into(), cx);
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });

        cx.simulate_mouse_down(
            point(px(8.), px(11.)),
            gpui::MouseButton::Right,
            gpui::Modifiers::none(),
        );
        assert!(
            cx.read_from_clipboard().is_none(),
            "the ellipsis must swallow the right click instead of copying the file path"
        );

        cx.simulate_mouse_down(
            point(px(300.), px(300.)),
            gpui::MouseButton::Right,
            gpui::Modifiers::none(),
        );
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("fallback".to_string()),
            "sanity: away from the ellipsis the same click reaches the fallback"
        );
    }
}
