mod directory;
mod symbol;

use std::rc::Rc;

use editor::{
    BREADCRUMB_PICKER_RENDERERS, BreadcrumbPickerRenderers, ErasedBreadcrumbPopoverHandle,
};
use gpui::App;

pub(crate) const MAX_BREADCRUMB_MENU_ENTRIES: usize = 200;

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
