//! Where a window goes when it changes monitors, or when it is restored from maximized: the
//! decisions only, in physical pixels, so they can be tested without a screen.
//!
//! The rule is the one Windows itself follows for apps that handle display scaling: a window
//! keeps its size in logical pixels, so text and buttons look the same size on every screen.
//! On top of that it must fit: on a screen too small for it, it takes [`MAX_SHARE`] of the work
//! area instead of hanging off the edges where its borders cannot be reached.
#![cfg_attr(not(windows), allow(dead_code))]

/// The most of a work area a window takes when it has to be made smaller to fit.
pub(crate) const MAX_SHARE: f64 = 0.88;
/// The scale Windows calls 100%.
pub(crate) const BASE_DPI: u32 = 96;

/// A rectangle in physical pixels: the right and bottom edges are outside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
}

impl Rect {
    pub(crate) fn new(left: i32, top: i32, width: i32, height: i32) -> Self {
        Self {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }

    pub(crate) fn width(&self) -> i32 {
        self.right - self.left
    }

    pub(crate) fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub(crate) fn offset(&self, x: i32, y: i32) -> Self {
        Self::new(self.left + x, self.top + y, self.width(), self.height())
    }
}

/// The monitor a window is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Screen {
    /// The part of the monitor not covered by the taskbar.
    pub(crate) work: Rect,
    pub(crate) dpi: u32,
    /// The smallest the window can be, with its frame, at this monitor's scale.
    pub(crate) min: (i32, i32),
}

/// `pixels` at `dpi`, in logical pixels.
pub(crate) fn logical(pixels: i32, dpi: u32) -> f64 {
    f64::from(pixels) * f64::from(BASE_DPI) / f64::from(dpi.max(1))
}

/// `logical` pixels at `dpi`, in physical pixels.
#[allow(clippy::cast_possible_truncation)]
fn physical(logical: f64, dpi: u32) -> i32 {
    (logical * f64::from(dpi) / f64::from(BASE_DPI)).round() as i32
}

/// A length that fits `room`: `wanted`, or [`MAX_SHARE`] of `room` if `wanted` is more than
/// `limit`, and never below `min` (unless `room` itself is smaller).
#[allow(clippy::cast_possible_truncation)]
fn fitting(wanted: i32, limit: i32, room: i32, min: i32) -> i32 {
    let length = if wanted > limit {
        (f64::from(room) * MAX_SHARE).round() as i32
    } else {
        wanted
    };
    length.max(min).min(room.max(1))
}

/// The cap [`fitting`] uses for a window that changed monitors: [`MAX_SHARE`] of the room.
#[allow(clippy::cast_possible_truncation)]
fn share_of(room: i32) -> i32 {
    (f64::from(room) * MAX_SHARE).round() as i32
}

/// `left`, moved just enough that a window `width` wide lies inside `from..to`.
fn inside(left: i32, width: i32, from: i32, to: i32) -> i32 {
    left.clamp(from, (to - width).max(from))
}

/// A window of `width` by `height` centred in the work area.
fn centred(width: i32, height: i32, work: Rect) -> Rect {
    Rect::new(
        work.left + (work.width() - width) / 2,
        work.top + (work.height() - height) / 2,
        width,
        height,
    )
}

/// A window that arrived on `screen` from another monitor, where it was `size` logical pixels:
/// that size again, made smaller if it does not fit, and moved inside the work area from where
/// it was dropped.
pub(crate) fn arrived(rect: Rect, size: (f64, f64), screen: &Screen) -> Rect {
    let work = screen.work;
    let fit = |logical_length: f64, room: i32, min: i32| {
        let wanted = physical(logical_length, screen.dpi);
        fitting(wanted, share_of(room), room, min)
    };
    let width = fit(size.0, work.width(), screen.min.0);
    let height = fit(size.1, work.height(), screen.min.1);
    Rect::new(
        inside(rect.left, width, work.left, work.right),
        inside(rect.top, height, work.top, work.bottom),
        width,
        height,
    )
}

/// A window that stays on its monitor: left as it is, unless it is larger than the work area.
/// Then it becomes [`MAX_SHARE`] of it, inside it.
pub(crate) fn stayed(rect: Rect, screen: &Screen) -> Rect {
    let work = screen.work;
    if rect.width() <= work.width() && rect.height() <= work.height() {
        return rect;
    }
    let width = fitting(rect.width(), work.width(), work.width(), screen.min.0);
    let height = fitting(rect.height(), work.height(), work.height(), screen.min.1);
    Rect::new(
        inside(rect.left, width, work.left, work.right),
        inside(rect.top, height, work.top, work.bottom),
        width,
        height,
    )
}

/// The size a maximized window on `screen` gets when it is restored. `normal` is the restore
/// size Windows kept, made on a monitor at `normal_dpi`; `same_monitor` says whether that was
/// this one. From another monitor it keeps its logical size, made smaller if it does not fit,
/// and is centred; on this monitor it is left alone unless larger than the work area.
pub(crate) fn restored(normal: Rect, normal_dpi: u32, same_monitor: bool, screen: &Screen) -> Rect {
    if same_monitor {
        let fits = normal.width() <= screen.work.width() && normal.height() <= screen.work.height();
        return if fits {
            normal
        } else {
            let placed = stayed(normal, screen);
            centred(placed.width(), placed.height(), screen.work)
        };
    }
    let size = (
        logical(normal.width(), normal_dpi),
        logical(normal.height(), normal_dpi),
    );
    let placed = arrived(normal, size, screen);
    centred(placed.width(), placed.height(), screen.work)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's laptop: 2944 x 1840 at 200%, the taskbar along the bottom.
    fn laptop() -> Screen {
        Screen {
            work: Rect::new(0, 0, 2944, 1744),
            dpi: 192,
            min: (1296, 900),
        }
    }

    /// A 1920 x 1080 monitor at 100%, to the right of the laptop.
    fn monitor() -> Screen {
        Screen {
            work: Rect::new(2944, 0, 1920, 1032),
            dpi: 96,
            min: (656, 459),
        }
    }

    fn logical_size(rect: Rect, dpi: u32) -> (f64, f64) {
        (logical(rect.width(), dpi), logical(rect.height(), dpi))
    }

    fn assert_inside(rect: Rect, work: Rect) {
        assert!(
            rect.left >= work.left && rect.right <= work.right,
            "{rect:?} in {work:?}"
        );
        assert!(
            rect.top >= work.top && rect.bottom <= work.bottom,
            "{rect:?} in {work:?}"
        );
    }

    #[test]
    fn a_window_moved_to_the_monitor_looks_the_same_size_not_bigger() {
        // 1089 x 663 logical on the laptop.
        let on_laptop = Rect::new(300, 200, 2178, 1326);
        let size = logical_size(on_laptop, 192);
        // Windows already scaled it as it crossed over.
        let dropped = Rect::new(3100, 150, 1089, 663);
        let placed = arrived(dropped, size, &monitor());
        assert_eq!(placed, dropped);
        // And when it did not, the result is the same.
        let unscaled = Rect::new(3100, 150, 2178, 1326);
        assert_eq!(arrived(unscaled, size, &monitor()).width(), 1089);
        assert_eq!(arrived(unscaled, size, &monitor()).height(), 663);
    }

    #[test]
    fn a_window_too_big_for_the_new_screen_is_made_to_fit_with_its_edges_reachable() {
        // Nearly the whole monitor: 1800 x 1000 logical, more than the laptop's 1472 x 872.
        let on_monitor = Rect::new(3000, 10, 1800, 1000);
        let size = logical_size(on_monitor, 96);
        let dropped = Rect::new(100, 50, 3600, 2000);
        let placed = arrived(dropped, size, &laptop());
        assert_inside(placed, laptop().work);
        assert_eq!(placed.width(), 2591);
        assert_eq!(placed.height(), 1535);
    }

    #[test]
    fn a_window_dropped_hanging_off_the_edge_is_pulled_inside() {
        let size = (1000., 600.);
        let dropped = Rect::new(4500, 700, 1000, 600);
        let placed = arrived(dropped, size, &monitor());
        assert_inside(placed, monitor().work);
        assert_eq!((placed.width(), placed.height()), (1000, 600));
    }

    #[test]
    fn it_never_becomes_smaller_than_the_smallest_window() {
        let placed = arrived(Rect::new(3000, 0, 100, 100), (100., 80.), &monitor());
        assert_eq!((placed.width(), placed.height()), monitor().min);
    }

    #[test]
    fn a_window_that_stays_is_left_alone_unless_larger_than_the_screen() {
        let fits = Rect::new(3000, 100, 1500, 900);
        assert_eq!(stayed(fits, &monitor()), fits);
        let too_big = Rect::new(2900, -20, 2178, 1326);
        let placed = stayed(too_big, &monitor());
        assert_inside(placed, monitor().work);
        assert_eq!((placed.width(), placed.height()), (1690, 908));
    }

    #[test]
    fn restoring_on_another_monitor_keeps_the_size_it_had_there_and_centres_it() {
        // Maximized on the monitor with a restore size made on the laptop.
        let normal = Rect::new(300, 200, 2178, 1326);
        let placed = restored(normal, 192, false, &monitor());
        assert_eq!((placed.width(), placed.height()), (1089, 663));
        assert_inside(placed, monitor().work);
        // And the other way: a large window from the monitor restored on the laptop.
        let normal = Rect::new(3000, 10, 1800, 1000);
        let placed = restored(normal, 96, false, &laptop());
        assert_inside(placed, laptop().work);
        assert!(placed.width() < laptop().work.width());
    }

    #[test]
    fn restoring_on_the_same_monitor_changes_nothing_that_fits() {
        let normal = Rect::new(3000, 100, 1200, 700);
        assert_eq!(restored(normal, 96, true, &monitor()), normal);
        let too_big = Rect::new(2944, 0, 2600, 1400);
        let placed = restored(too_big, 96, true, &monitor());
        assert_inside(placed, monitor().work);
    }
}
