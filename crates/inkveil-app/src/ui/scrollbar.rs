//! A thin scrollbar over the right edge of a scrolling area: it shows how far the area is
//! scrolled, and can be dragged or clicked to scroll it. It is only there while there is
//! something to scroll.

use std::{cell::Cell, rc::Rc};

use gpui::{
    App, Bounds, CursorStyle, DispatchPhase, Hitbox, HitboxBehavior, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollHandle, Styled, Window, canvas,
    fill, point, px, size,
};

use crate::theme::theme;

/// How wide the strip is that reacts to the mouse.
const WIDTH: f32 = 12.;
/// How wide the thumb is drawn inside it.
const THUMB_WIDTH: f32 = 6.;
/// The thumb never gets shorter than this, so it stays easy to take hold of.
const MIN_THUMB: f32 = 28.;

/// A scrolling area's position, and its scrollbar. Give [`Self::handle`] to the area with
/// `track_scroll`, and put [`Self::render`] after it inside a `relative` element.
#[derive(Clone, Default)]
pub struct Scrollbar {
    handle: ScrollHandle,
    /// While the thumb is being dragged: how far below its top the mouse took hold of it.
    grab: Rc<Cell<Option<Pixels>>>,
}

impl std::fmt::Debug for Scrollbar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scrollbar")
            .field("dragging", &self.dragging())
            .finish_non_exhaustive()
    }
}

/// Where the thumb is: its top and height inside a track `track` high, for an area `view` high
/// that can scroll `furthest` and has scrolled `scrolled`. `None` when nothing can scroll.
fn thumb(
    track: Pixels,
    view: Pixels,
    furthest: Pixels,
    scrolled: Pixels,
) -> Option<(Pixels, Pixels)> {
    if furthest <= px(0.) || view <= px(0.) || track <= px(MIN_THUMB) {
        return None;
    }
    let height = (track * (view / (view + furthest))).max(px(MIN_THUMB));
    let top = (track - height) * (scrolled.clamp(px(0.), furthest) / furthest);
    Some((top, height))
}

/// How far the area is scrolled when the thumb's top is at `top`.
fn scrolled_at(top: Pixels, track: Pixels, height: Pixels, furthest: Pixels) -> Pixels {
    let room = track - height;
    if room <= px(0.) {
        return px(0.);
    }
    furthest * (top.clamp(px(0.), room) / room)
}

impl Scrollbar {
    pub fn new() -> Self {
        Self::default()
    }

    /// The scroll position, for `track_scroll` and for reading or moving it.
    pub fn handle(&self) -> &ScrollHandle {
        &self.handle
    }

    /// Whether the thumb is being dragged right now.
    pub fn dragging(&self) -> bool {
        self.grab.get().is_some()
    }

    fn thumb(&self, track: Pixels) -> Option<(Pixels, Pixels)> {
        thumb(
            track,
            self.handle.bounds().size.height,
            self.handle.max_offset().y,
            -self.handle.offset().y,
        )
    }

    /// Scrolls so that the thumb's top is `top` below the top of the track.
    fn move_thumb(&self, top: Pixels, track: Pixels, height: Pixels) {
        let scrolled = scrolled_at(top, track, height, self.handle.max_offset().y);
        self.handle
            .set_offset(point(self.handle.offset().x, -scrolled));
    }

    /// The bar. It covers the right edge of the `relative` element it is put in.
    pub fn render(&self, cx: &App) -> impl IntoElement + use<> {
        let color = theme(cx).muted;
        let (for_prepaint, this) = (self.clone(), self.clone());
        canvas(
            move |bounds, window, _| {
                // It only takes the mouse away from what is under it when it shows.
                for_prepaint
                    .thumb(bounds.size.height)
                    .map(|_| window.insert_hitbox(bounds, HitboxBehavior::BlockMouseExceptScroll))
            },
            move |bounds, hitbox, window, _| {
                if let Some(hitbox) = hitbox {
                    this.paint(bounds, &hitbox, color, window);
                }
            },
        )
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(px(WIDTH))
    }

    fn paint(
        &self,
        track: Bounds<Pixels>,
        hitbox: &Hitbox,
        color: gpui::Hsla,
        window: &mut Window,
    ) {
        let Some((top, height)) = self.thumb(track.size.height) else {
            return;
        };
        window.set_cursor_style(CursorStyle::Arrow, hitbox);
        let strong = self.dragging() || hitbox.is_hovered(window);
        let left = track.left() + (track.size.width - px(THUMB_WIDTH)) / 2.;
        window.paint_quad(
            fill(
                Bounds::new(
                    point(left, track.top() + top),
                    size(px(THUMB_WIDTH), height),
                ),
                color.opacity(if strong { 0.9 } else { 0.45 }),
            )
            .corner_radii(px(THUMB_WIDTH / 2.)),
        );

        let (this, hitbox) = (self.clone(), hitbox.clone());
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }
            let at = event.position.y - track.top();
            // On the thumb, the drag starts where it was taken hold of. Beside it, the thumb
            // jumps to the mouse and is dragged by its middle.
            let grab = if at >= top && at <= top + height {
                at - top
            } else {
                this.move_thumb(at - height / 2., track.size.height, height);
                height / 2.
            };
            this.grab.set(Some(grab));
            window.refresh();
            cx.stop_propagation();
        });
        let this = self.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, _, window, _| {
            let Some(grab) = this.grab.get() else {
                return;
            };
            if event.pressed_button == Some(MouseButton::Left) {
                let at = event.position.y - track.top();
                this.move_thumb(at - grab, track.size.height, height);
            } else {
                // The button was let go outside the window.
                this.grab.set(None);
            }
            window.refresh();
        });
        let this = self.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, _, window, _| {
            if this.grab.take().is_some() {
                window.refresh();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_to_scroll_means_no_thumb() {
        assert_eq!(thumb(px(400.), px(400.), px(0.), px(0.)), None);
    }

    #[test]
    fn the_thumb_shows_what_part_is_on_screen() {
        // Half of the text is on screen: the thumb is half the track.
        assert_eq!(
            thumb(px(400.), px(400.), px(400.), px(0.)),
            Some((px(0.), px(200.)))
        );
        // Scrolled to the end: the thumb touches the bottom.
        assert_eq!(
            thumb(px(400.), px(400.), px(400.), px(400.)),
            Some((px(200.), px(200.)))
        );
        // A very long text still has a thumb big enough to take hold of.
        let (_, height) = thumb(px(400.), px(400.), px(400_000.), px(0.)).unwrap();
        assert_eq!(height, px(MIN_THUMB));
    }

    #[test]
    fn dragging_the_thumb_scrolls_in_proportion_and_stops_at_the_ends() {
        let (track, height, furthest) = (px(400.), px(200.), px(400.));
        assert_eq!(scrolled_at(px(100.), track, height, furthest), px(200.));
        assert_eq!(scrolled_at(px(-50.), track, height, furthest), px(0.));
        assert_eq!(scrolled_at(px(900.), track, height, furthest), furthest);
        // Where the thumb is drawn and where a drag puts it agree.
        let (top, height) = thumb(track, px(400.), furthest, px(120.)).unwrap();
        let back = scrolled_at(top, track, height, furthest);
        assert!((back - px(120.)).abs() < px(0.01), "{back:?}");
    }
}
