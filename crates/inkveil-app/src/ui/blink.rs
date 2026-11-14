//! A caret that blinks like in any Windows text box: on and off about once a second, and
//! solid for a moment after typing or moving, so it is never off where the eye looks for it.

use std::time::{Duration, Instant};

use gpui::{Context, Task};

/// How long the caret is on, and then off: the Windows default.
const HALF_PERIOD: Duration = Duration::from_millis(530);

/// The blinking of one view's caret.
pub struct Blink {
    /// When the caret last moved: it is on from then for half a period, then off, and so on.
    since: Instant,
    /// Redraws the view each time the caret turns on or off, while it has the keyboard.
    task: Option<Task<()>>,
}

impl std::fmt::Debug for Blink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Blink")
            .field("running", &self.task.is_some())
            .finish_non_exhaustive()
    }
}

/// Whether the caret is on, `elapsed` after it last moved.
fn on_after(elapsed: Duration) -> bool {
    (elapsed.as_millis() / HALF_PERIOD.as_millis()).is_multiple_of(2)
}

/// How long until the caret next turns on or off, `elapsed` after it last moved.
fn until_flip(elapsed: Duration) -> Duration {
    let into = elapsed.as_millis() % HALF_PERIOD.as_millis();
    HALF_PERIOD.saturating_sub(Duration::from_millis(u64::try_from(into).unwrap_or(0)))
}

impl Blink {
    pub fn new() -> Self {
        Self {
            since: Instant::now(),
            task: None,
        }
    }

    /// The caret moved or the text changed: it shows solid again for half a period.
    pub fn reset(&mut self) {
        self.since = Instant::now();
    }

    /// Whether to draw the caret right now.
    pub fn visible(&self) -> bool {
        on_after(self.since.elapsed())
    }

    /// Keeps the view redrawing in step with the caret while `active` (it has the keyboard and
    /// its window is in front), and stops when it is not. Call it from `render`. `blink` finds
    /// this value in the view.
    pub fn run<T: 'static>(
        &mut self,
        active: bool,
        blink: fn(&mut T) -> &mut Self,
        cx: &mut Context<T>,
    ) {
        if !active {
            self.task = None;
            return;
        }
        if self.task.is_some() {
            return;
        }
        self.reset();
        self.task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(wait) = this.update(cx, |view, _| until_flip(blink(view).since.elapsed()))
                else {
                    break;
                };
                // A little past the flip, so the redraw sees the new state.
                cx.background_executor()
                    .timer(wait + Duration::from_millis(8))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caret_is_on_right_after_moving_then_off_then_on_again() {
        assert!(on_after(Duration::ZERO));
        assert!(on_after(Duration::from_millis(500)));
        assert!(!on_after(Duration::from_millis(600)));
        assert!(on_after(Duration::from_millis(1100)));
    }

    #[test]
    fn the_next_redraw_is_at_the_next_flip() {
        assert_eq!(until_flip(Duration::ZERO), HALF_PERIOD);
        assert_eq!(
            until_flip(Duration::from_millis(500)),
            Duration::from_millis(30)
        );
        assert_eq!(
            until_flip(Duration::from_millis(600)),
            Duration::from_millis(460)
        );
    }
}
