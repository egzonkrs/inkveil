//! The custom GPUI element that lays out and paints a document.

use gpui::{
    App, AvailableSpace, Bounds, DispatchPhase, Element, ElementId, ElementInputHandler, Entity,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, MouseMoveEvent, Pixels, Size,
    Style, TextRun, Window, fill, point, px, relative, size,
};
use inkveil_doc::{Loc, Pos};

use super::{
    EditorSettings, EditorView,
    layout::{BlockContent, DocLayout, Marker, TextLayout},
};
use crate::theme::{self, Theme, theme};

/// Horizontal padding around the document inside the scroll view (left + right).
pub const TEXT_PADDING: f32 = 60.;

pub struct EditorElement {
    view: Entity<EditorView>,
}

impl EditorElement {
    pub fn new(view: Entity<EditorView>) -> Self {
        Self { view }
    }
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let wrap = EditorSettings::get(cx).wrap;
        let mut style = Style::default();
        if wrap {
            style.size.width = relative(1.).into();
        } else {
            // Without wrapping the element takes the width of its longest line, and at least the
            // view's width, so the scroll container can scroll sideways.
            style.min_size.width = relative(1.).into();
        }
        let view = self.view.clone();
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, cx| {
                let viewport = view.read(cx).scroll.bounds().size.width - px(TEXT_PADDING);
                let width = match (known.width, available.width) {
                    (Some(w), _) if wrap => w,
                    (_, AvailableSpace::Definite(w)) if wrap => w,
                    // Unwrapped text is aligned within the visible width.
                    _ if viewport > px(0.) => viewport,
                    (Some(w), _) | (_, AvailableSpace::Definite(w)) => w,
                    _ => px(600.),
                };
                let theme = theme(cx);
                let settings = EditorSettings::get(cx);
                let (height, content_width) = view.update(cx, |view, _| {
                    let decorations = view.decorations(settings.spellcheck);
                    view.layout.update(
                        view.editor.doc(),
                        width,
                        settings,
                        &decorations,
                        &theme,
                        window,
                    );
                    (view.layout.height, view.layout.content_width)
                });
                Size {
                    // Without wrapping, long lines make the document wider than the view.
                    width: if settings.wrap {
                        width
                    } else {
                        width.max(content_width + px(24.))
                    },
                    height,
                }
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let scrolled = self.view.update(cx, |view, _| {
            view.bounds = Some(bounds);
            view.bounds_scroll_offset = view.scroll.offset();
            view.autoscroll && {
                view.autoscroll = false;
                scroll_to_caret(view, bounds)
            }
        });
        if scrolled {
            // This frame is already laid out at the old offset. Notifying the view now would not
            // schedule another frame (GPUI ignores that while drawing), so ask for one directly.
            let view = self.view.entity_id();
            window.on_next_frame(move |_, cx| cx.notify(view));
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let colors = theme(cx);
        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        // Mouse moves over the editor only reach its own handlers; drags (selecting, resizing a
        // picture, the scrollbars) must keep following the pointer once it leaves.
        let view = self.view.downgrade();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Capture {
                view.update(cx, |view, cx| view.pointer_move(event, window, cx))
                    .ok();
            }
        });
        let focused = focus_handle.is_focused(window);

        // Borrow the layout out of the view so painting can use `cx` freely.
        let (layout, selection, others, doc_empty, has_find) = self.view.update(cx, |view, _| {
            (
                std::mem::replace(&mut view.layout, DocLayout::empty()),
                view.editor.selection(),
                view.editor.selections().split_off(1),
                view.editor.doc().is_empty(),
                !view.find_matches.is_empty(),
            )
        });
        let visible = window.content_mask().bounds;
        let ranges: Vec<(Pos, Pos)> = std::iter::once(selection)
            .chain(others.iter().copied())
            .map(|s| s.ordered())
            .filter(|(a, b)| a != b)
            .collect();

        for (index, block) in layout.blocks.iter().enumerate() {
            let top = bounds.top() + block.top;
            if top > visible.bottom() || top + block.height < visible.top() {
                continue;
            }
            let origin = point(bounds.left() + block.left, top);
            match &block.content {
                BlockContent::Text {
                    text,
                    marker,
                    quote,
                    code,
                } => {
                    let box_width = bounds.size.width - block.left;
                    if *code {
                        window.paint_quad(
                            fill(
                                Bounds::new(origin, size(box_width, block.height)),
                                colors.code_background,
                            )
                            .corner_radii(px(6.)),
                        );
                    }
                    if *quote {
                        window.paint_quad(fill(
                            Bounds::new(origin + point(px(2.), px(0.)), size(px(3.), block.height)),
                            colors.border,
                        ));
                    }
                    paint_marker(marker, text, origin, &colors, window, cx);
                    let loc = Loc::block(index);
                    let text_origin = origin + text.origin;
                    if has_find {
                        self.paint_find(text, text_origin, loc, &colors, window, cx);
                    }
                    for (start, end) in &ranges {
                        paint_selection(text, text_origin, loc, *start, *end, &colors, window);
                    }
                    text.paint(text_origin, window, cx);
                }
                BlockContent::Image {
                    blob,
                    size: shown,
                    x,
                    ..
                } => {
                    let image_bounds =
                        Bounds::new(point(bounds.left() + block.left + *x, top), *shown);
                    let hovered = self.view.read(cx).hovered_image == Some(index);
                    match crate::images::ImageCache::get(inkveil_store::BlobId(*blob), cx) {
                        crate::images::Lookup::Ready(image) => {
                            let _ = window.paint_image(
                                image_bounds,
                                image_bounds,
                                gpui::Corners::all(px(6.)),
                                image,
                                0,
                                false,
                            );
                        }
                        crate::images::Lookup::Loading | crate::images::Lookup::Failed => {
                            window.paint_quad(
                                fill(image_bounds, colors.code_background).corner_radii(px(6.)),
                            );
                        }
                    }
                    if hovered {
                        // Outline and the resize handle in the bottom-right corner.
                        window.paint_quad(
                            gpui::outline(image_bounds, colors.accent, gpui::BorderStyle::Solid)
                                .corner_radii(px(6.)),
                        );
                        let handle = Bounds::new(
                            image_bounds.bottom_right() - point(px(14.), px(14.)),
                            size(px(10.), px(10.)),
                        );
                        window.paint_quad(fill(handle, colors.accent).corner_radii(px(2.)));
                    }
                }
                BlockContent::Divider => {
                    let y = top + block.height / 2.;
                    window.paint_quad(fill(
                        Bounds::new(point(bounds.left(), y), size(bounds.size.width, px(1.))),
                        colors.border,
                    ));
                }
                BlockContent::Table(table) => {
                    for (r, row) in table.cells.iter().enumerate() {
                        for (c, cell) in row.iter().enumerate() {
                            let cell_ref = inkveil_doc::CellRef {
                                row: u16::try_from(r).unwrap_or(0),
                                col: u16::try_from(c).unwrap_or(0),
                            };
                            let text_origin = origin + table.cell_origin(cell_ref);
                            let loc = Loc {
                                block: index,
                                cell: Some(cell_ref),
                            };
                            if has_find {
                                self.paint_find(cell, text_origin, loc, &colors, window, cx);
                            }
                            for (start, end) in &ranges {
                                paint_selection(
                                    cell,
                                    text_origin,
                                    loc,
                                    *start,
                                    *end,
                                    &colors,
                                    window,
                                );
                            }
                            cell.paint(text_origin, window, cx);
                        }
                    }
                    paint_table_grid(table, origin, &colors, window);
                }
            }
        }

        let viewport = self.view.read(cx).scroll.bounds();
        paint_line_numbers(
            &layout,
            bounds,
            viewport,
            selection.head,
            visible,
            &colors,
            window,
            cx,
        );

        if doc_empty && let Some(first) = layout.blocks.first().and_then(|b| b.text()) {
            let placeholder = "Start writing…";
            let run = TextRun {
                len: placeholder.len(),
                font: gpui::font(theme::UI_FONT),
                color: colors.muted,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line =
                window
                    .text_system()
                    .shape_line(placeholder.into(), first.font_size, &[run], None);
            let _ = line.paint(
                bounds.origin + point(layout.gutter, px(0.)) + first.origin,
                first.line_height,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            );
        }

        if focused {
            for head in std::iter::once(selection.head).chain(others.iter().map(|s| s.head)) {
                if let Some((caret, height)) = layout.caret(head) {
                    window.paint_quad(fill(
                        Bounds::new(bounds.origin + caret, size(px(1.5), height)),
                        colors.caret,
                    ));
                }
            }
        }
        self.view.update(cx, |view, _| view.layout = layout);
    }
}

impl EditorElement {
    /// Paints find result highlights for one text.
    fn paint_find(
        &self,
        text: &TextLayout,
        origin: gpui::Point<Pixels>,
        loc: Loc,
        colors: &Theme,
        window: &mut Window,
        cx: &App,
    ) {
        let line_height = text.line_height;
        for (range, current) in self.view.read(cx).find_highlights_at(loc) {
            let color = if current {
                colors.find_current
            } else {
                colors.find_match
            };
            for (p, width) in text.selection_rects(range) {
                window.paint_quad(
                    fill(Bounds::new(origin + p, size(width, line_height)), color)
                        .corner_radii(px(2.)),
                );
            }
        }
    }
}

/// Paints line numbers right-aligned in the gutter; the caret's line is brighter.
#[allow(clippy::too_many_arguments)]
fn paint_line_numbers(
    layout: &DocLayout,
    bounds: Bounds<Pixels>,
    viewport: Bounds<Pixels>,
    caret: Pos,
    visible: Bounds<Pixels>,
    colors: &Theme,
    window: &mut Window,
    cx: &mut App,
) {
    if layout.gutter <= px(0.) {
        return;
    }
    // The gutter stays at the left edge of the view when the text scrolls sideways, on a
    // background strip so scrolled text passes underneath it.
    let left = bounds.left().max(viewport.left());
    if left > bounds.left() {
        window.paint_quad(fill(
            Bounds::new(
                point(viewport.left(), visible.top()),
                size(left - viewport.left() + layout.gutter, visible.size.height),
            ),
            colors.background,
        ));
    }
    let caret_top = layout.caret(caret).map(|(p, _)| p.y);
    let current = caret_top.and_then(|y| {
        layout
            .line_numbers
            .iter()
            .rev()
            .find(|(top, _, _)| *top <= y + px(1.))
            .map(|(_, _, n)| *n)
    });
    for (top, line_height, number) in &layout.line_numbers {
        let y = bounds.top() + *top;
        if y > visible.bottom() || y + *line_height < visible.top() {
            continue;
        }
        let label = number.to_string();
        let run = TextRun {
            len: label.len(),
            font: gpui::font(theme::UI_FONT),
            color: if Some(*number) == current {
                colors.text
            } else {
                colors.muted
            },
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_line(label.into(), px(12.5), &[run], None);
        let x = left + layout.gutter - px(16.) - line.width();
        let _ = line.paint(
            point(x, y),
            *line_height,
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

/// Paints the selection highlight for one text, if the selection touches it.
fn paint_selection(
    text: &TextLayout,
    origin: gpui::Point<Pixels>,
    loc: Loc,
    start: Pos,
    end: Pos,
    colors: &Theme,
    window: &mut Window,
) {
    if start == end {
        return;
    }
    let here = Pos { loc, offset: 0 };
    let here_end = Pos {
        loc,
        offset: usize::MAX,
    };
    if end < here || start > here_end {
        return;
    }
    let from = if start.loc == loc { start.offset } else { 0 };
    let to = if end.loc == loc {
        end.offset
    } else {
        usize::MAX
    };
    let line_height = text.line_height;
    for (p, width) in text.selection_rects(from..to) {
        window.paint_quad(fill(
            Bounds::new(origin + p, size(width, line_height)),
            colors.selection,
        ));
    }
}

fn paint_marker(
    marker: &Marker,
    text: &TextLayout,
    origin: gpui::Point<Pixels>,
    colors: &Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let line_height = text.line_height;
    match marker {
        Marker::None => {}
        Marker::Bullet(indent) => {
            let d = px(5.5);
            let center = origin + point(px(10.), line_height / 2.);
            let quad = fill(
                Bounds::new(center - point(d / 2., d / 2.), size(d, d)),
                colors.text,
            );
            let quad = match indent % 3 {
                0 => quad.corner_radii(d / 2.),
                1 => quad
                    .corner_radii(d / 2.)
                    .border_widths(px(1.2))
                    .border_color(colors.text)
                    .background(gpui::transparent_black()),
                _ => quad,
            };
            window.paint_quad(quad);
        }
        Marker::Number(label) => {
            let run = TextRun {
                len: label.len(),
                font: gpui::font(theme::UI_FONT),
                color: colors.muted,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line =
                window
                    .text_system()
                    .shape_line(label.clone().into(), text.font_size, &[run], None);
            let x = origin.x + px(20.) - line.width();
            let _ = line.paint(
                point(x, origin.y),
                line_height,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        Marker::Checkbox(checked) => {
            let s = px(14.);
            let at = origin + point(px(2.), (line_height - s) / 2.);
            let mut quad = fill(
                Bounds::new(at, size(s, s)),
                if *checked {
                    colors.accent
                } else {
                    gpui::transparent_black()
                },
            )
            .corner_radii(px(3.));
            if !*checked {
                quad = quad.border_widths(px(1.5)).border_color(colors.muted);
            }
            window.paint_quad(quad);
            if *checked {
                let run = TextRun {
                    len: "✓".len(),
                    font: gpui::font(theme::UI_FONT),
                    color: gpui::white(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window
                    .text_system()
                    .shape_line("✓".into(), px(11.), &[run], None);
                let x = at.x + (s - line.width()) / 2.;
                let _ = line.paint(point(x, at.y), s, gpui::TextAlign::Left, None, window, cx);
            }
        }
    }
}

fn paint_table_grid(
    table: &super::layout::TableLayout,
    origin: gpui::Point<Pixels>,
    colors: &Theme,
    window: &mut Window,
) {
    let height = table.height();
    for x in table.col_x.iter().copied().chain([table.width]) {
        window.paint_quad(fill(
            Bounds::new(
                origin + point(x.min(table.width - px(1.)), px(0.)),
                size(px(1.), height),
            ),
            colors.border,
        ));
    }
    for y in table.row_y.iter().copied().chain([height]) {
        window.paint_quad(fill(
            Bounds::new(
                origin + point(px(0.), y.min(height - px(1.))),
                size(table.width, px(1.)),
            ),
            colors.border,
        ));
    }
}

/// Adjusts the scroll offset so the caret is visible.
fn scroll_to_caret(view: &EditorView, bounds: Bounds<Pixels>) -> bool {
    let Some((caret, height)) = view.layout.caret(view.editor.selection().head) else {
        return false;
    };
    let viewport = view.scroll.bounds();
    if viewport.size.height <= px(0.) {
        return false;
    }
    let original = view.scroll.offset();
    let mut offset = original;

    let margin = height;
    let caret_top = bounds.top() + caret.y;
    let caret_bottom = caret_top + height;
    if caret_top - margin < viewport.top() {
        offset.y += viewport.top() - (caret_top - margin);
    } else if caret_bottom + margin > viewport.bottom() {
        offset.y -= caret_bottom + margin - viewport.bottom();
    }

    // Sideways too, for long lines when wrapping is off.
    let side_margin = px(48.);
    let caret_x = bounds.left() + caret.x;
    if caret_x - side_margin < viewport.left() {
        offset.x += viewport.left() - (caret_x - side_margin);
    } else if caret_x + side_margin > viewport.right() {
        offset.x -= caret_x + side_margin - viewport.right();
    }

    let max = view.scroll.max_offset();
    offset.x = offset.x.clamp(-max.x, px(0.));
    offset.y = offset.y.clamp(-max.y, px(0.));
    if offset == original {
        false
    } else {
        view.scroll.set_offset(offset);
        true
    }
}
