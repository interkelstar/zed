use gpui::{
    AnyElement, App, Context, EventEmitter, Global, IntoElement, Render, Subscription, Window,
};
use ui::prelude::*;
use workspace::{
    ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView,
    item::{HighlightedText, ItemEvent, ItemHandle},
};

type RenderBreadcrumbTextFn =
    fn(Vec<HighlightedText>, Option<AnyElement>, &dyn ItemHandle, bool, &App) -> AnyElement;

pub struct RenderBreadcrumbText(pub RenderBreadcrumbTextFn);

impl Global for RenderBreadcrumbText {}

pub struct Breadcrumbs {
    pane_focused: bool,
    active_item: Option<Box<dyn ItemHandle>>,
    subscription: Option<Subscription>,
}

impl Default for Breadcrumbs {
    fn default() -> Self {
        Self::new()
    }
}

impl Breadcrumbs {
    pub fn new() -> Self {
        Self {
            pane_focused: false,
            active_item: Default::default(),
            subscription: Default::default(),
        }
    }
}

impl EventEmitter<ToolbarItemEvent> for Breadcrumbs {}

impl Render for Breadcrumbs {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The trail's own scroll container lives in `render_breadcrumb_text`.
        let element = h_flex()
            .id("breadcrumb-container")
            .flex_grow_1()
            .h_8()
            .text_ui(cx);

        let Some(active_item) = self.active_item.as_ref() else {
            return element.into_any_element();
        };

        let Some((segments, font)) = active_item.breadcrumbs(cx) else {
            return element.into_any_element();
        };

        let prefix_element = active_item.breadcrumb_prefix(window, cx);

        let Some(render_fn) = cx.try_global::<RenderBreadcrumbText>() else {
            return element.into_any_element();
        };
        let content = (render_fn.0)(segments, prefix_element, active_item.as_ref(), false, cx);
        match font {
            Some(font) => div()
                .flex_grow_1()
                .min_w_0()
                .font(font)
                .child(content)
                .into_any_element(),
            None => content,
        }
    }
}

impl ToolbarItemView for Breadcrumbs {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        cx.notify();
        let switching_away = self.active_item.as_ref().is_some_and(|previous| {
            Some(previous.item_id()) != active_pane_item.map(ItemHandle::item_id)
        });
        let previous_item = self.active_item.take();
        if switching_away && let Some(previous_item) = previous_item {
            previous_item.breadcrumb_cancel_reanchor(cx);
        }

        let Some(item) = active_pane_item else {
            return ToolbarItemLocation::Hidden;
        };

        let this = cx.entity().downgrade();
        self.subscription = Some(item.subscribe_to_item_events(
            window,
            cx,
            Box::new(move |event, _, cx| {
                if let ItemEvent::UpdateBreadcrumbs = event {
                    this.update(cx, |this, cx| {
                        cx.notify();
                        if let Some(active_item) = this.active_item.as_ref() {
                            cx.emit(ToolbarItemEvent::ChangeLocation(
                                active_item.breadcrumb_location(cx),
                            ))
                        }
                    })
                    .ok();
                }
            }),
        ));
        self.active_item = Some(item.boxed_clone());
        item.breadcrumb_location(cx)
    }

    fn pane_focus_update(
        &mut self,
        pane_focused: bool,
        _window: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.pane_focused = pane_focused;
    }
}
