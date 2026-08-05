/// Parent is the nearest preceding entry with a smaller depth, not `depth - 1`: tree-sitter outlines can jump depth unevenly.
fn outline_parents(depths: &[usize]) -> Vec<Option<usize>> {
    let mut parents = Vec::with_capacity(depths.len());
    let mut ancestor_stack: Vec<(usize, usize)> = Vec::new();
    for (index, &depth) in depths.iter().enumerate() {
        while ancestor_stack
            .last()
            .is_some_and(|&(ancestor_depth, _)| ancestor_depth >= depth)
        {
            ancestor_stack.pop();
        }
        parents.push(ancestor_stack.last().map(|&(_, parent_index)| parent_index));
        ancestor_stack.push((depth, index));
    }
    parents
}

pub fn sibling_outline_indices(depths: &[usize], target_index: usize) -> Vec<usize> {
    if target_index >= depths.len() {
        return Vec::new();
    }

    let parents = outline_parents(depths);
    let target_parent = parents[target_index];
    parents
        .iter()
        .enumerate()
        .filter_map(|(index, &parent)| (parent == target_parent).then_some(index))
        .collect()
}

pub fn child_outline_indices(depths: &[usize], target_index: usize) -> Vec<usize> {
    if target_index >= depths.len() {
        return Vec::new();
    }

    let parents = outline_parents(depths);
    parents
        .iter()
        .enumerate()
        .filter_map(|(index, &parent)| (parent == Some(target_index)).then_some(index))
        .collect()
}

pub fn top_level_outline_indices(depths: &[usize]) -> Vec<usize> {
    let parents = outline_parents(depths);
    parents
        .iter()
        .enumerate()
        .filter_map(|(index, &parent)| parent.is_none().then_some(index))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sibling_outline_indices_top_level() {
        let depths = [0, 0, 0];
        assert_eq!(sibling_outline_indices(&depths, 0), vec![0, 1, 2]);
        assert_eq!(sibling_outline_indices(&depths, 1), vec![0, 1, 2]);
        assert_eq!(sibling_outline_indices(&depths, 2), vec![0, 1, 2]);
    }

    #[test]
    fn test_sibling_outline_indices_nested() {
        // `impl A { fn one; fn two }` then `impl B { fn three }`.
        let depths = [0, 1, 1, 0, 1];
        assert_eq!(sibling_outline_indices(&depths, 1), vec![1, 2]);
        assert_eq!(sibling_outline_indices(&depths, 2), vec![1, 2]);
        assert_eq!(sibling_outline_indices(&depths, 4), vec![4]);
        assert_eq!(sibling_outline_indices(&depths, 0), vec![0, 3]);
        assert_eq!(sibling_outline_indices(&depths, 3), vec![0, 3]);
    }

    #[test]
    fn test_sibling_outline_indices_uneven_depths() {
        let depths = [0, 2, 2, 0];
        assert_eq!(sibling_outline_indices(&depths, 1), vec![1, 2]);
        assert_eq!(sibling_outline_indices(&depths, 2), vec![1, 2]);
        assert_eq!(sibling_outline_indices(&depths, 0), vec![0, 3]);
    }

    #[test]
    fn test_sibling_outline_indices_single_item() {
        let depths = [0];
        assert_eq!(sibling_outline_indices(&depths, 0), vec![0]);
    }

    #[test]
    fn test_sibling_outline_indices_out_of_bounds() {
        let depths = [0, 0];
        assert_eq!(sibling_outline_indices(&depths, 5), Vec::<usize>::new());
    }

    #[test]
    fn test_child_outline_indices_top_level() {
        let depths = [0, 0, 0];
        assert_eq!(child_outline_indices(&depths, 0), Vec::<usize>::new());
        assert_eq!(child_outline_indices(&depths, 1), Vec::<usize>::new());
        assert_eq!(child_outline_indices(&depths, 2), Vec::<usize>::new());
    }

    #[test]
    fn test_child_outline_indices_nested() {
        // `impl A { fn one; fn two }` then `impl B { fn three }`.
        let depths = [0, 1, 1, 0, 1];
        assert_eq!(child_outline_indices(&depths, 0), vec![1, 2]);
        assert_eq!(child_outline_indices(&depths, 3), vec![4]);
        assert_eq!(child_outline_indices(&depths, 1), Vec::<usize>::new());
        assert_eq!(child_outline_indices(&depths, 2), Vec::<usize>::new());
        assert_eq!(child_outline_indices(&depths, 4), Vec::<usize>::new());
    }

    #[test]
    fn test_child_outline_indices_uneven_depths() {
        let depths = [0, 2, 2, 0];
        assert_eq!(child_outline_indices(&depths, 0), vec![1, 2]);
        assert_eq!(child_outline_indices(&depths, 3), Vec::<usize>::new());
    }

    #[test]
    fn test_child_outline_indices_out_of_bounds() {
        let depths = [0, 0];
        assert_eq!(child_outline_indices(&depths, 5), Vec::<usize>::new());
    }

    #[test]
    fn test_top_level_outline_indices() {
        let depths = [0, 1, 1, 0, 1];
        assert_eq!(top_level_outline_indices(&depths), vec![0, 3]);

        let depths_uneven = [0, 2, 2, 0];
        assert_eq!(top_level_outline_indices(&depths_uneven), vec![0, 3]);

        let depths_empty: [usize; 0] = [];
        assert_eq!(
            top_level_outline_indices(&depths_empty),
            Vec::<usize>::new()
        );
    }
}
