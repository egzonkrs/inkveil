//! Small reusable UI pieces.

pub mod text_field;

use gpui::{
    AnyView, App, AppContext as _, Context, ElementId, IntoElement, ParentElement, Render,
    SharedString, Stateful, Styled, Window, div, prelude::*, px, svg,
};

use crate::{assets::Icon, theme::theme};

/// An icon in `color` (SVG icons do not inherit the text color).
pub fn icon(icon: Icon, color: gpui::Hsla) -> gpui::Svg {
    svg()
        .path(icon.path())
        .size(px(16.))
        .flex_none()
        .text_color(color)
}

/// A square toolbar button with an icon. `active` shows it as switched on.
pub fn icon_button(
    id: impl Into<ElementId>,
    which: Icon,
    active: bool,
    tooltip: &'static str,
    cx: &App,
) -> Stateful<gpui::Div> {
    let colors = theme(cx);
    let color = if active { colors.accent } else { colors.icon };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(28.))
        .rounded_md()
        .cursor_pointer()
        .text_color(if active { colors.accent } else { colors.icon })
        .when(active, |d| d.bg(colors.selected))
        .hover(|d| d.bg(colors.hover))
        .tooltip(move |_, cx| Tooltip::view(tooltip, cx))
        .child(icon(which, color))
}

/// A text button. Primary buttons are filled with the accent color.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    primary: bool,
    cx: &App,
) -> Stateful<gpui::Div> {
    let colors = theme(cx);
    let label = label.into();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(32.))
        .px_4()
        .rounded_md()
        .cursor_pointer()
        .text_size(px(14.))
        .when(primary, |d| {
            d.bg(colors.accent)
                .text_color(gpui::white())
                .hover(|d| d.opacity(0.9))
        })
        .when(!primary, |d| {
            d.border_1()
                .border_color(colors.border)
                .text_color(colors.text)
                .hover(|d| d.bg(colors.hover))
        })
        .child(label)
}

/// A one-line hover tooltip.
pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    pub fn view(text: impl Into<SharedString>, cx: &mut App) -> AnyView {
        let text = text.into();
        cx.new(|_| Self { text }).into()
    }
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme(cx);
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .text_color(colors.text)
            .text_size(px(12.))
            .child(self.text.clone())
    }
}

/// A horizontal hairline separator.
pub fn hairline(cx: &App) -> gpui::Div {
    div().h(px(1.)).w_full().flex_none().bg(theme(cx).border)
}

/// A thin vertical separator for toolbars.
pub fn toolbar_separator(cx: &App) -> gpui::Div {
    div()
        .w(px(1.))
        .h(px(18.))
        .mx_1()
        .flex_none()
        .bg(theme(cx).border)
}
