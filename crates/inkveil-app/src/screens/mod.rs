//! Full-window screens shown before the workspace.

pub mod setup;
pub mod unlock;

use gpui::{App, IntoElement, ParentElement, Styled, div, px};

use crate::theme::theme;

/// The app mark and name, shown at the top of the setup and unlock screens.
pub fn brand(cx: &App) -> impl IntoElement {
    let _ = theme(cx);
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(gpui::img("images/inkveil.png").size(px(64.)).rounded_xl())
        .child(
            div()
                .text_size(px(22.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Inkveil"),
        )
}

/// A centered column used by both screens.
pub fn centered_card() -> gpui::Div {
    div().size_full().flex().items_center().justify_center()
}
