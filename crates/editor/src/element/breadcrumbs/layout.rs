use super::*;

/// Where a segment sits in [`plan_breadcrumb_layout`]'s drop order when the bar can't fit everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BreadcrumbSegmentKind {
    Root,
    Middle,
    File,
    Symbol,
}

pub(crate) fn classify_breadcrumb_segment_kinds(
    segment_count: usize,
    file_segment_index: usize,
    has_root_segment: bool,
) -> Vec<BreadcrumbSegmentKind> {
    (0..segment_count)
        .map(|index| match index.cmp(&file_segment_index) {
            Ordering::Greater => BreadcrumbSegmentKind::Symbol,
            Ordering::Equal => BreadcrumbSegmentKind::File,
            Ordering::Less if has_root_segment && index == 0 => BreadcrumbSegmentKind::Root,
            Ordering::Less => BreadcrumbSegmentKind::Middle,
        })
        .collect()
}

/// Replaces `symbol_segments` wholesale if its length disagrees with `segments`: later steps assume equal length and would panic in `Vec::splice` otherwise.
pub(crate) fn align_symbol_segments(
    segments: &[HighlightedText],
    symbol_segments: Vec<Option<BreadcrumbSegmentTarget>>,
) -> Vec<Option<BreadcrumbSegmentTarget>> {
    if symbol_segments.len() == segments.len() {
        symbol_segments
    } else {
        vec![None; segments.len()]
    }
}

const MAX_BREADCRUMB_SEGMENTS_HARD_CAP: usize = 64;

/// The cap is display-only, so an expanded row bypasses it: expansion must reveal every segment.
pub(crate) fn hard_cap_breadcrumb_middle_segments(
    mut segments: Vec<HighlightedText>,
    mut symbol_segments: Vec<Option<BreadcrumbSegmentTarget>>,
    mut kinds: Vec<BreadcrumbSegmentKind>,
    mut file_segment_index: usize,
    expanded: bool,
) -> (
    Vec<HighlightedText>,
    Vec<Option<BreadcrumbSegmentTarget>>,
    Vec<BreadcrumbSegmentKind>,
    usize,
    Option<usize>,
) {
    let middle_start = kinds
        .iter()
        .position(|kind| *kind == BreadcrumbSegmentKind::Middle);
    let middle_end = kinds
        .iter()
        .rposition(|kind| *kind == BreadcrumbSegmentKind::Middle)
        .map(|index| index + 1);
    let (Some(middle_start), Some(middle_end)) = (middle_start, middle_end) else {
        return (segments, symbol_segments, kinds, file_segment_index, None);
    };
    if expanded || middle_end - middle_start <= MAX_BREADCRUMB_SEGMENTS_HARD_CAP {
        return (segments, symbol_segments, kinds, file_segment_index, None);
    }

    let half = MAX_BREADCRUMB_SEGMENTS_HARD_CAP / 2;
    let splice_start = middle_start + half;
    let splice_end = middle_end - half;

    segments.splice(
        splice_start..splice_end,
        Some(HighlightedText {
            text: "⋯".into(),
            highlights: vec![],
        }),
    );
    symbol_segments.splice(splice_start..splice_end, Some(None));
    kinds.splice(
        splice_start..splice_end,
        Some(BreadcrumbSegmentKind::Middle),
    );

    // `File` always follows every `Middle` segment, so this splice can only shift its index left.
    file_segment_index -= (splice_end - splice_start) - 1;

    (
        segments,
        symbol_segments,
        kinds,
        file_segment_index,
        Some(splice_start),
    )
}

/// `visible` and `ellipses` together partition `0..segment_count`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BreadcrumbLayoutPlan {
    pub(crate) visible: Vec<usize>,
    pub(crate) ellipses: Vec<Range<usize>>,
}

fn total_breadcrumb_layout_width(metrics: &BreadcrumbSegmentMetrics, dropped: &[bool]) -> Pixels {
    let mut total = Pixels::ZERO;
    let mut items = 0usize;
    let mut in_dropped_run = false;
    for (index, &is_dropped) in dropped.iter().enumerate() {
        if is_dropped {
            if !in_dropped_run {
                total += metrics.ellipsis_width;
                items += 1;
                in_dropped_run = true;
            }
        } else {
            total += metrics.widths[index];
            items += 1;
            in_dropped_run = false;
        }
    }
    // The trailing item paints no separator.
    total + metrics.separator_width * items.saturating_sub(1) as f32
}

fn breadcrumb_layout_plan_from_dropped(dropped: &[bool]) -> BreadcrumbLayoutPlan {
    let mut visible = Vec::new();
    let mut ellipses = Vec::new();
    let mut run_start = None;
    for (index, &is_dropped) in dropped.iter().enumerate() {
        if is_dropped {
            run_start.get_or_insert(index);
        } else {
            if let Some(start) = run_start.take() {
                ellipses.push(start..index);
            }
            visible.push(index);
        }
    }
    if let Some(start) = run_start {
        ellipses.push(start..dropped.len());
    }
    BreadcrumbLayoutPlan { visible, ellipses }
}

/// Drops segments cheapest first (`Middle`, `Root`, then `File`/`Symbol` in the order `file_outlives_symbols` picks); the last one and `anchored_index` never drop.
pub(crate) fn plan_breadcrumb_layout(
    metrics: &BreadcrumbSegmentMetrics,
    kinds: &[BreadcrumbSegmentKind],
    available_width: Pixels,
    anchored_index: Option<usize>,
    file_outlives_symbols: bool,
) -> BreadcrumbLayoutPlan {
    debug_assert_eq!(metrics.widths.len(), kinds.len());
    let segment_count = metrics.widths.len();
    if segment_count == 0 {
        return BreadcrumbLayoutPlan {
            visible: Vec::new(),
            ellipses: Vec::new(),
        };
    }

    let mut dropped = vec![false; segment_count];
    if total_breadcrumb_layout_width(metrics, &dropped) <= available_width {
        return breadcrumb_layout_plan_from_dropped(&dropped);
    }

    let last_index = segment_count - 1;
    let mut drop_order = Vec::with_capacity(segment_count - 1);
    let last_two_kinds = if file_outlives_symbols {
        [BreadcrumbSegmentKind::Symbol, BreadcrumbSegmentKind::File]
    } else {
        [BreadcrumbSegmentKind::File, BreadcrumbSegmentKind::Symbol]
    };
    for kind in [
        BreadcrumbSegmentKind::Middle,
        BreadcrumbSegmentKind::Root,
        last_two_kinds[0],
        last_two_kinds[1],
    ] {
        drop_order.extend(
            kinds
                .iter()
                .enumerate()
                .filter_map(|(index, segment_kind)| {
                    (index != last_index && Some(index) != anchored_index && *segment_kind == kind)
                        .then_some(index)
                }),
        );
    }

    for index in drop_order {
        dropped[index] = true;
        if total_breadcrumb_layout_width(metrics, &dropped) <= available_width {
            break;
        }
    }

    breadcrumb_layout_plan_from_dropped(&dropped)
}

pub(crate) fn breadcrumb_layout_plan_width(
    metrics: &BreadcrumbSegmentMetrics,
    plan: &BreadcrumbLayoutPlan,
) -> Pixels {
    let mut dropped = vec![false; metrics.widths.len()];
    for range in &plan.ellipses {
        for index in range.clone() {
            dropped[index] = true;
        }
    }
    total_breadcrumb_layout_width(metrics, &dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_symbol_segments_realigns_divergent_lengths() {
        let segments: Vec<HighlightedText> = (0..3)
            .map(|i| HighlightedText {
                text: format!("segment-{i}").into(),
                highlights: vec![],
            })
            .collect();
        let symbol_segments = vec![Some(BreadcrumbSegmentTarget::Symbol {
            buffer_id: BufferId::new(1).unwrap(),
            item: None,
            is_active_segment: false,
        })];

        let symbol_segments = align_symbol_segments(&segments, symbol_segments);

        assert_eq!(symbol_segments.len(), 3);
        assert!(symbol_segments.iter().all(Option::is_none));
    }

    #[test]
    fn test_classify_breadcrumb_segment_kinds() {
        let kinds = classify_breadcrumb_segment_kinds(6, 3, true);
        assert_eq!(
            kinds,
            vec![
                BreadcrumbSegmentKind::Root,
                BreadcrumbSegmentKind::Middle,
                BreadcrumbSegmentKind::Middle,
                BreadcrumbSegmentKind::File,
                BreadcrumbSegmentKind::Symbol,
                BreadcrumbSegmentKind::Symbol,
            ]
        );

        let kinds = classify_breadcrumb_segment_kinds(3, 1, false);
        assert_eq!(
            kinds,
            vec![
                BreadcrumbSegmentKind::Middle,
                BreadcrumbSegmentKind::File,
                BreadcrumbSegmentKind::Symbol,
            ]
        );

        let kinds = classify_breadcrumb_segment_kinds(1, 0, false);
        assert_eq!(kinds, vec![BreadcrumbSegmentKind::File]);
    }

    /// Without `align_symbol_segments`, a short `symbol_segments` panics: splices below assume `segments.len()`.
    #[test]
    fn test_hard_cap_breadcrumb_middle_segments_does_not_panic_on_divergent_symbol_segments() {
        let segments: Vec<HighlightedText> = (0..100)
            .map(|i| HighlightedText {
                text: format!("segment-{i}").into(),
                highlights: vec![],
            })
            .collect();
        let symbol_segments = vec![Some(BreadcrumbSegmentTarget::Symbol {
            buffer_id: BufferId::new(1).unwrap(),
            item: None,
            is_active_segment: false,
        })];
        let symbol_segments = align_symbol_segments(&segments, symbol_segments);
        assert_eq!(symbol_segments.len(), segments.len());

        let kinds = classify_breadcrumb_segment_kinds(segments.len(), 99, true);

        let (capped_segments, capped_symbol_segments, capped_kinds, file_segment_index, cap_index) =
            hard_cap_breadcrumb_middle_segments(
                segments.clone(),
                symbol_segments.clone(),
                kinds.clone(),
                99,
                false,
            );

        // 67 = root (1) + capped middle (32 prefix + 1 "⋯" + 32 suffix) + file (1).
        assert_eq!(capped_segments.len(), 67);
        assert_eq!(capped_symbol_segments.len(), capped_segments.len());
        assert_eq!(capped_kinds.len(), capped_segments.len());
        assert_eq!(file_segment_index, 66);
        assert_eq!(
            capped_kinds[file_segment_index],
            BreadcrumbSegmentKind::File
        );
        assert_eq!(
            cap_index,
            Some(33),
            "the pseudo-segment sits after the root and the 32 kept prefix segments"
        );
        assert_eq!(capped_segments[33].text.as_ref(), "⋯");

        let (segments, symbol_segments, kinds, file_segment_index, cap_index) =
            hard_cap_breadcrumb_middle_segments(segments, symbol_segments, kinds, 99, true);
        assert_eq!(
            segments.len(),
            100,
            "an expanded row bypasses the cap so expansion reveals every segment"
        );
        assert_eq!(symbol_segments.len(), 100);
        assert_eq!(kinds.len(), 100);
        assert_eq!(file_segment_index, 99);
        assert_eq!(cap_index, None);
    }

    #[test]
    fn test_hard_cap_breadcrumb_middle_segments_leaves_ordinary_input_untouched() {
        let segments: Vec<HighlightedText> = (0..6)
            .map(|i| HighlightedText {
                text: format!("segment-{i}").into(),
                highlights: vec![],
            })
            .collect();
        let symbol_segments = vec![None; segments.len()];
        let kinds = classify_breadcrumb_segment_kinds(segments.len(), 3, true);

        let (segments, symbol_segments, kinds, file_segment_index, cap_index) =
            hard_cap_breadcrumb_middle_segments(segments, symbol_segments, kinds, 3, false);

        assert_eq!(segments.len(), 6);
        assert_eq!(symbol_segments.len(), 6);
        assert_eq!(kinds.len(), 6);
        assert_eq!(file_segment_index, 3);
        assert_eq!(cap_index, None);
    }

    /// Separator-free, so each case's threshold is the sum of the widths it names.
    fn metrics(widths: Vec<Pixels>, ellipsis_width: Pixels) -> BreadcrumbSegmentMetrics {
        BreadcrumbSegmentMetrics {
            widths,
            ellipsis_width,
            separator_width: Pixels::ZERO,
        }
    }

    fn sample_breadcrumb_widths_and_kinds() -> (Vec<Pixels>, Vec<BreadcrumbSegmentKind>) {
        use BreadcrumbSegmentKind::*;
        let widths = vec![
            px(60.),  // root
            px(30.),  // a
            px(30.),  // b
            px(30.),  // c
            px(30.),  // d
            px(80.),  // file.kt
            px(90.),  // Class
            px(120.), // fun method
        ];
        let kinds = vec![Root, Middle, Middle, Middle, Middle, File, Symbol, Symbol];
        (widths, kinds)
    }

    #[test]
    fn test_plan_breadcrumb_layout_counts_separators_between_items_only() {
        use BreadcrumbSegmentKind::*;
        let metrics = BreadcrumbSegmentMetrics {
            widths: vec![px(40.), px(40.), px(40.)],
            ellipsis_width: px(20.),
            separator_width: px(20.),
        };
        let kinds = vec![Root, Middle, File];

        let plan = plan_breadcrumb_layout(&metrics, &kinds, px(160.), None, false);
        assert_eq!(
            plan.visible,
            vec![0, 1, 2],
            "three segments and the two separators between them fit in 160px"
        );
        assert_eq!(breadcrumb_layout_plan_width(&metrics, &plan), px(160.));

        let plan = plan_breadcrumb_layout(&metrics, &kinds, px(159.), None, false);
        assert_eq!(plan.visible, vec![0, 2], "a pixel short drops the middle");
        assert_eq!(
            breadcrumb_layout_plan_width(&metrics, &plan),
            px(140.),
            "the ellipsis takes the dropped segment's place, so two separators remain"
        );
    }

    #[test]
    fn test_plan_breadcrumb_layout_drops_middle_before_root_before_file_before_outer_symbols() {
        let (widths, kinds) = sample_breadcrumb_widths_and_kinds();
        let total: Pixels = widths.iter().fold(Pixels::ZERO, |sum, w| sum + *w);

        let cases = [
            (total, (0..widths.len()).collect::<Vec<_>>(), Vec::new()),
            (px(380.), vec![0, 5, 6, 7], vec![1..5]),
            (px(340.), vec![5, 6, 7], vec![0..5]),
            (px(230.), vec![6, 7], vec![0..6]),
            (px(140.), vec![7], vec![0..7]),
            // Degenerate width: the last segment always survives.
            (px(1.), vec![7], vec![0..7]),
        ];
        for (available, visible, ellipses) in cases {
            let plan = plan_breadcrumb_layout(
                &metrics(widths.clone(), px(20.)),
                &kinds,
                available,
                None,
                false,
            );
            assert_eq!(plan.visible, visible, "available width {available:?}");
            assert_eq!(plan.ellipses, ellipses, "available width {available:?}");
        }
    }

    /// Hidden tab bar: the bar holds the only copy of the file name, so it outlives the symbols.
    #[test]
    fn test_plan_breadcrumb_layout_keeps_the_file_segment_over_symbols_when_tab_bar_is_hidden() {
        let (widths, kinds) = sample_breadcrumb_widths_and_kinds();

        let plan = plan_breadcrumb_layout(&metrics(widths, px(20.)), &kinds, px(240.), None, true);
        assert_eq!(
            plan.visible,
            vec![5, 7],
            "the file segment (5) survives while the inner symbol (6) drops"
        );
        assert_eq!(plan.ellipses, vec![0..5, 6..7]);
    }

    #[test]
    fn test_plan_breadcrumb_layout_boundary_inputs() {
        let plan = plan_breadcrumb_layout(
            &metrics(vec![px(500.)], px(20.)),
            &[BreadcrumbSegmentKind::File],
            px(1.),
            None,
            false,
        );
        assert_eq!(plan.visible, vec![0], "a single segment never collapses");
        assert!(plan.ellipses.is_empty());

        let plan =
            plan_breadcrumb_layout(&metrics(Vec::new(), px(20.)), &[], px(500.), None, false);
        assert!(plan.visible.is_empty());
        assert!(plan.ellipses.is_empty());
    }

    /// What `breadcrumb_leaf_navigation` relies on: the keyboard-opened dropdown's segment is on screen at any width.
    #[test]
    fn test_plan_breadcrumb_layout_keeps_the_anchored_leaf_at_any_width() {
        let (widths, kinds) = sample_breadcrumb_widths_and_kinds();
        let leaf = kinds.len() - 1;

        let plan =
            plan_breadcrumb_layout(&metrics(widths, px(20.)), &kinds, px(1.), Some(leaf), false);

        assert_eq!(plan.visible, vec![leaf]);
        assert_eq!(plan.ellipses, vec![0..leaf]);
    }

    #[test]
    fn test_plan_breadcrumb_layout_keeps_the_anchored_middle_segment() {
        use BreadcrumbSegmentKind::*;
        let widths = vec![px(50.), px(40.), px(40.), px(40.), px(60.)];
        let kinds = vec![Root, Middle, Middle, Middle, File];

        let plan =
            plan_breadcrumb_layout(&metrics(widths, px(20.)), &kinds, px(140.), Some(2), false);

        assert_eq!(
            plan.visible,
            vec![2, 4],
            "the anchored middle segment survives while its neighbours collapse"
        );
        assert_eq!(plan.ellipses, vec![0..2, 3..4]);
    }
}
