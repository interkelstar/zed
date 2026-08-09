mod directory;
mod symbol;

use std::rc::Rc;

use editor::{
    BREADCRUMB_PICKER_RENDERERS, BreadcrumbPickerRenderers, ErasedBreadcrumbPopoverHandle,
};
use fuzzy::{StringMatch, StringMatchCandidate};
use gpui::App;

pub(crate) const MAX_BREADCRUMB_MENU_ENTRIES: usize = 200;

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
        })
        .ok();
}

fn default_popover_handle() -> Rc<dyn ErasedBreadcrumbPopoverHandle> {
    Rc::new(directory::DirectoryPopoverHandle(Default::default()))
}

fn default_symbol_popover_handle() -> Rc<dyn ErasedBreadcrumbPopoverHandle> {
    Rc::new(symbol::SymbolPopoverHandle(Default::default()))
}

#[cfg(test)]
pub(crate) mod test_support {
    use editor::Editor;
    use gpui::{Context, Entity, IntoElement, Render, TestAppContext, Window};
    use settings::KeymapFile;

    /// `PopoverMenu`-free pickers need a real `Render` root to drive keystrokes through.
    pub(crate) struct Harness<P: Render> {
        pub(crate) picker: Entity<P>,
        pub(crate) editor: Entity<Editor>,
    }

    impl<P: Render> Render for Harness<P> {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.picker.clone()
        }
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
