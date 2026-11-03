//! Turning document blocks into positioned, shaped text.
//!
//! Each block is laid out on its own and cached. Blocks are copy-on-write, so after an edit only
//! the blocks whose allocation changed are shaped again; typing in a long note re-shapes one
//! paragraph.

use std::{ops::Range, sync::Arc};

use gpui::{
    App, Bounds, Font, FontStyle, FontWeight, Hsla, Pixels, Point, SharedString, Size,
    StrikethroughStyle, TextAlign, TextRun, UnderlineStyle, Window, WrappedLine, point, px, size,
};
use inkveil_doc::{
    Align, Block, CellRef, Document, Loc, Marks, Pos, RichText, TextBlock, TextKind, diff_blocks,
};
use smallvec::SmallVec;

use super::EditorSettings;
use crate::theme::{self, Theme};

/// Body text size.
pub const BASE_FONT_SIZE: f32 = 15.;
const LINE_HEIGHT: f32 = 1.55;
const INDENT_STEP: f32 = 24.;
const LIST_GUTTER: f32 = 24.;
const QUOTE_GUTTER: f32 = 16.;
const CODE_PADDING: f32 = 10.;
const BLOCK_GAP: f32 = 4.;
const HEADING_GAP: f32 = 12.;
const DIVIDER_HEIGHT: f32 = 21.;
const CELL_PAD_X: f32 = 8.;
const CELL_PAD_Y: f32 = 5.;
const MIN_COL_WIDTH: f32 = 60.;

/// Inputs that change how text looks, beyond the document itself.
#[derive(Clone, PartialEq)]
pub struct Decorations {
    /// Misspelled ranges per location, painted with a wavy underline.
    pub spelling: Vec<(Loc, Range<usize>)>,
    /// The IME composition range, underlined.
    pub marked: Option<(Loc, Range<usize>)>,
}

/// One visual row of wrapped text.
#[derive(Debug, Clone, Copy)]
pub struct Row {
    /// Which hard line (split at `\n`) the row belongs to.
    line: usize,
    /// Byte range in the block text.
    start: usize,
    end: usize,
    /// Top of the row relative to the text origin.
    y: Pixels,
    /// x of the row's first glyph in the unwrapped line.
    start_x: Pixels,
    /// Width of the row's glyphs.
    width: Pixels,
}

/// Shaped text of one text block or table cell.
pub struct TextLayout {
    /// Text origin relative to the block (or cell) origin.
    pub origin: Point<Pixels>,
    pub wrap_width: Pixels,
    pub line_height: Pixels,
    pub font_size: Pixels,
    pub align: TextAlign,
    lines: SmallVec<[WrappedLine; 1]>,
    line_starts: SmallVec<[usize; 1]>,
    rows: Vec<Row>,
    text_len: usize,
}

impl TextLayout {
    /// Shapes `text`. With `wrap` off, lines run past `wrap_width`, which then only serves
    /// alignment.
    #[allow(clippy::too_many_arguments)]
    fn shape(
        text: &RichText,
        style: &TextStyle,
        wrap_width: Pixels,
        wrap: bool,
        loc: Loc,
        decorations: &Decorations,
        theme: &Theme,
        window: &mut Window,
    ) -> Self {
        let line_height = (style.font_size * LINE_HEIGHT).round();
        let runs = text_runs(text, style, loc, decorations, theme);
        let shaped = window
            .text_system()
            .shape_text(
                SharedString::from(text.as_str().to_owned()),
                style.font_size,
                &runs,
                wrap.then_some(wrap_width),
                None,
            )
            .unwrap_or_default();

        let mut line_starts = SmallVec::new();
        let mut start = 0;
        for line in text.as_str().split('\n') {
            line_starts.push(start);
            start += line.len() + 1;
        }

        let mut rows = Vec::new();
        let mut y = px(0.);
        for (line_ix, line) in shaped.iter().enumerate() {
            let base = line_starts.get(line_ix).copied().unwrap_or(0);
            let unwrapped = &line.unwrapped_layout;
            let mut row_start = 0;
            let mut row_start_x = px(0.);
            for boundary in &line.wrap_boundaries {
                let glyph = &unwrapped.runs[boundary.run_ix].glyphs[boundary.glyph_ix];
                rows.push(Row {
                    line: line_ix,
                    start: base + row_start,
                    end: base + glyph.index,
                    y,
                    start_x: row_start_x,
                    width: glyph.position.x - row_start_x,
                });
                row_start = glyph.index;
                row_start_x = glyph.position.x;
                y += line_height;
            }
            rows.push(Row {
                line: line_ix,
                start: base + row_start,
                end: base + unwrapped.len,
                y,
                start_x: row_start_x,
                width: unwrapped.width - row_start_x,
            });
            y += line_height;
        }
        if rows.is_empty() {
            rows.push(Row {
                line: 0,
                start: 0,
                end: 0,
                y: px(0.),
                start_x: px(0.),
                width: px(0.),
            });
        }

        Self {
            origin: point(px(0.), px(0.)),
            wrap_width,
            line_height,
            font_size: style.font_size,
            align: match style.align {
                Align::Left => TextAlign::Left,
                Align::Center => TextAlign::Center,
                Align::Right => TextAlign::Right,
            },
            lines: shaped,
            line_starts,
            rows,
            text_len: text.len(),
        }
    }

    pub fn height(&self) -> Pixels {
        self.line_height * self.rows.len() as f32
    }

    /// Width of the widest visual row.
    pub fn max_row_width(&self) -> Pixels {
        self.rows
            .iter()
            .map(|r| r.width + self.x_shift(r))
            .fold(px(0.), Pixels::max)
    }

    /// Top of each hard line (text separated by line breaks), relative to the text origin.
    pub fn line_tops(&self) -> impl Iterator<Item = Pixels> + '_ {
        self.rows
            .iter()
            .enumerate()
            .filter(|(i, row)| *i == 0 || self.rows[i - 1].line != row.line)
            .map(|(_, row)| row.y)
    }

    fn x_shift(&self, row: &Row) -> Pixels {
        match self.align {
            TextAlign::Left => px(0.),
            TextAlign::Center => ((self.wrap_width - row.width) / 2.).max(px(0.)),
            TextAlign::Right => (self.wrap_width - row.width).max(px(0.)),
        }
    }

    fn row_for_offset(&self, offset: usize) -> &Row {
        // At a wrap point the caret belongs to the start of the next row.
        self.rows
            .iter()
            .rev()
            .find(|r| r.start <= offset)
            .unwrap_or(&self.rows[0])
    }

    fn x_in_row(&self, row: &Row, offset: usize) -> Pixels {
        let Some(line) = self.lines.get(row.line) else {
            return self.x_shift(row);
        };
        let local = offset.saturating_sub(self.line_starts[row.line]);
        line.unwrapped_layout.x_for_index(local) - row.start_x + self.x_shift(row)
    }

    /// Caret position for `offset`, relative to the text origin.
    pub fn position_for_offset(&self, offset: usize) -> Point<Pixels> {
        let row = self.row_for_offset(offset.min(self.text_len));
        point(self.x_in_row(row, offset), row.y)
    }

    /// The offset closest to `position` (relative to the text origin).
    pub fn offset_for_position(&self, position: Point<Pixels>) -> usize {
        // Non-negative and clamped to the row count right after, so the cast cannot misbehave.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let index =
            ((position.y / self.line_height).floor().max(0.) as usize).min(self.rows.len() - 1);
        let row = &self.rows[index];
        let Some(line) = self.lines.get(row.line) else {
            return row.start;
        };
        let line_start = self.line_starts[row.line];
        let x = position.x - self.x_shift(row) + row.start_x;
        let local = line.unwrapped_layout.closest_index_for_x(x.max(px(0.)));
        let mut offset = (line_start + local).clamp(row.start, row.end);
        let is_last_row_of_line = self
            .rows
            .get(index + 1)
            .is_none_or(|next| next.line != row.line);
        if offset == row.end && !is_last_row_of_line && row.end > row.start {
            // Clicking past a wrapped row's end puts the caret before its trailing space.
            offset = row.end - 1;
        }
        offset
    }

    /// Start and end offsets of the visual row containing `offset`.
    pub fn row_bounds(&self, offset: usize) -> (usize, usize) {
        let row = self.row_for_offset(offset);
        let is_last = self
            .rows
            .iter()
            .position(|r| std::ptr::eq(r, row))
            .and_then(|i| self.rows.get(i + 1))
            .is_none_or(|next| next.line != row.line);
        let end = if is_last || row.end == row.start {
            row.end
        } else {
            row.end - 1
        };
        (row.start, end)
    }

    /// Rectangles covering `range`, relative to the text origin.
    pub fn selection_rects(&self, range: Range<usize>) -> Vec<(Point<Pixels>, Pixels)> {
        let mut rects = Vec::new();
        for row in &self.rows {
            if range.end < row.start || range.start > row.end {
                continue;
            }
            let from = range.start.max(row.start);
            let to = range.end.min(row.end);
            let x0 = self.x_in_row(row, from);
            let mut x1 = self.x_in_row(row, to);
            if range.end > row.end {
                // The selection continues past this row: show it running to the row end.
                x1 = x1.max(x0 + px(6.));
            }
            if x1 > x0 {
                rects.push((point(x0, row.y), x1 - x0));
            }
        }
        rects
    }

    pub fn paint(&self, origin: Point<Pixels>, window: &mut Window, cx: &mut App) {
        let mut y = px(0.);
        for line in &self.lines {
            let rows = line.wrap_boundaries.len() + 1;
            let line_origin = origin + point(px(0.), y);
            let bounds = gpui::Bounds::new(
                line_origin,
                gpui::size(self.wrap_width, self.line_height * rows as f32),
            );
            let _ = line.paint_background(
                line_origin,
                self.line_height,
                self.align,
                Some(bounds),
                window,
                cx,
            );
            let _ = line.paint(
                line_origin,
                self.line_height,
                self.align,
                Some(bounds),
                window,
                cx,
            );
            y += self.line_height * rows as f32;
        }
    }
}

/// How a block's text is styled.
struct TextStyle {
    font_size: Pixels,
    font: Font,
    color: Hsla,
    align: Align,
    code: bool,
}

fn block_text_style(block: &TextBlock, theme: &Theme) -> TextStyle {
    let scale = match block.kind {
        TextKind::Heading(1) => 1.8,
        TextKind::Heading(2) => 1.45,
        TextKind::Heading(_) => 1.2,
        _ => block.size.scale(),
    };
    let code = block.kind == TextKind::Code;
    let mut font = gpui::font(if code {
        theme::MONO_FONT
    } else {
        theme::UI_FONT
    });
    if matches!(block.kind, TextKind::Heading(_)) {
        font.weight = FontWeight::SEMIBOLD;
    }
    let size = if code {
        BASE_FONT_SIZE * 0.9
    } else {
        BASE_FONT_SIZE * scale
    };
    let checked = matches!(block.kind, TextKind::Task { checked: true });
    TextStyle {
        font_size: px(size),
        font,
        color: if checked || block.kind == TextKind::Quote {
            theme.muted
        } else {
            theme.text
        },
        align: block.align,
        code,
    }
}

fn cell_text_style(theme: &Theme) -> TextStyle {
    TextStyle {
        font_size: px(BASE_FONT_SIZE * 0.95),
        font: gpui::font(theme::UI_FONT),
        color: theme.text,
        align: Align::Left,
        code: false,
    }
}

/// Converts marks into GPUI text runs, splitting where spelling or IME ranges begin and end.
fn text_runs(
    text: &RichText,
    style: &TextStyle,
    loc: Loc,
    decorations: &Decorations,
    theme: &Theme,
) -> Vec<TextRun> {
    let spelling: Vec<&Range<usize>> = decorations
        .spelling
        .iter()
        .filter(|(l, _)| *l == loc)
        .map(|(_, r)| r)
        .collect();
    let marked = decorations
        .marked
        .as_ref()
        .filter(|(l, _)| *l == loc)
        .map(|(_, r)| r);

    let mut cuts: Vec<usize> = vec![0, text.len()];
    cuts.extend(text.run_ranges().map(|(r, _)| r.end));
    for r in spelling.iter().copied().chain(marked) {
        cuts.push(r.start.min(text.len()));
        cuts.push(r.end.min(text.len()));
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut runs = Vec::with_capacity(cuts.len());
    let mut mark_runs = text.run_ranges().peekable();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start == end {
            continue;
        }
        while mark_runs.peek().is_some_and(|(r, _)| r.end <= start) {
            mark_runs.next();
        }
        let marks = mark_runs.peek().map_or(&Marks::NONE, |(_, m)| *m);
        let misspelled = spelling.iter().any(|r| r.start <= start && end <= r.end);
        let composing = marked.is_some_and(|r| r.start <= start && end <= r.end);
        runs.push(run_for(
            end - start,
            marks,
            style,
            misspelled,
            composing,
            theme,
        ));
    }
    if runs.is_empty() {
        runs.push(run_for(0, &Marks::NONE, style, false, false, theme));
    }
    runs
}

fn run_for(
    len: usize,
    marks: &Marks,
    style: &TextStyle,
    misspelled: bool,
    composing: bool,
    theme: &Theme,
) -> TextRun {
    let mut font = if marks.code && !style.code {
        gpui::font(theme::MONO_FONT)
    } else {
        style.font.clone()
    };
    if marks.bold {
        font.weight = FontWeight::BOLD;
    }
    if marks.italic {
        font.style = FontStyle::Italic;
    }
    let color = if marks.link.is_some() {
        theme.accent
    } else {
        marks.color.map_or(style.color, |c| gpui::rgb(c.0).into())
    };
    let background = if let Some(h) = marks.highlight {
        Some(gpui::rgba((h.0 << 8) | 0x60).into())
    } else if marks.code && !style.code {
        Some(theme.code_background)
    } else {
        None
    };
    let underline = if misspelled {
        Some(UnderlineStyle {
            color: Some(theme.error),
            thickness: px(1.),
            wavy: true,
        })
    } else if composing || marks.underline || marks.link.is_some() {
        Some(UnderlineStyle {
            color: Some(color),
            thickness: px(1.),
            wavy: false,
        })
    } else {
        None
    };
    TextRun {
        len,
        font,
        color,
        background_color: background,
        underline,
        strikethrough: marks.strike.then(|| StrikethroughStyle {
            color: Some(color),
            thickness: px(1.),
        }),
    }
}

/// Layout of a table.
pub struct TableLayout {
    pub col_x: Vec<Pixels>,
    pub row_y: Vec<Pixels>,
    pub row_heights: Vec<Pixels>,
    pub width: Pixels,
    pub cells: Vec<Vec<TextLayout>>,
}

impl TableLayout {
    pub fn cell_origin(&self, cell: CellRef) -> Point<Pixels> {
        point(
            self.col_x[usize::from(cell.col)] + px(CELL_PAD_X),
            self.row_y[usize::from(cell.row)] + px(CELL_PAD_Y),
        )
    }

    pub fn cell_at(&self, position: Point<Pixels>) -> CellRef {
        let col = self
            .col_x
            .iter()
            .rposition(|x| *x <= position.x)
            .unwrap_or(0);
        let row = self
            .row_y
            .iter()
            .rposition(|y| *y <= position.y)
            .unwrap_or(0);
        CellRef {
            row: u16::try_from(row).unwrap_or(0),
            col: u16::try_from(col).unwrap_or(0),
        }
    }

    pub fn height(&self) -> Pixels {
        self.row_heights.iter().copied().fold(px(0.), |a, b| a + b)
    }
}

/// What a block looks like once laid out.
pub enum BlockContent {
    Text {
        text: TextLayout,
        /// List marker or checkbox drawn in the gutter.
        marker: Marker,
        quote: bool,
        code: bool,
    },
    Divider,
    Table(TableLayout),
    Image {
        blob: [u8; 16],
        /// Displayed size.
        size: Size<Pixels>,
        /// Offset from the block's left edge (alignment).
        x: Pixels,
        /// Width of the text column, the basis of the width percentage.
        column: Pixels,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Marker {
    None,
    Bullet(u8),
    Number(String),
    Checkbox(bool),
}

pub struct BlockLayout {
    /// Space above the block.
    pub margin_top: Pixels,
    /// Height of the block's own box.
    pub height: Pixels,
    /// Left edge of the block's box (indent).
    pub left: Pixels,
    pub content: BlockContent,
    /// Top of the block's box in document coordinates (filled in by `DocLayout`).
    pub top: Pixels,
}

impl BlockLayout {
    pub fn text(&self) -> Option<&TextLayout> {
        match &self.content {
            BlockContent::Text { text, .. } => Some(text),
            _ => None,
        }
    }

    /// The text layout at `loc` within this block and its origin relative to the block top-left.
    pub fn text_at(&self, cell: Option<CellRef>) -> Option<(&TextLayout, Point<Pixels>)> {
        match (&self.content, cell) {
            (BlockContent::Text { text, .. }, None) => {
                Some((text, point(self.left, px(0.)) + text.origin))
            }
            (BlockContent::Table(table), Some(cell)) => {
                let text = table
                    .cells
                    .get(usize::from(cell.row))?
                    .get(usize::from(cell.col))?;
                Some((text, point(self.left, px(0.)) + table.cell_origin(cell)))
            }
            _ => None,
        }
    }
}

/// Width of the line number gutter.
pub const LINE_NUMBER_GUTTER: f32 = 44.;

/// The laid-out document.
pub struct DocLayout {
    width: Pixels,
    scale_factor: f32,
    settings: EditorSettings,
    decorations: Decorations,
    dark: bool,
    source: Vec<Arc<Block>>,
    pub blocks: Vec<BlockLayout>,
    pub height: Pixels,
    /// Width of the widest content; more than the view width when wrapping is off.
    pub content_width: Pixels,
    /// Space reserved left of the text for line numbers.
    pub gutter: Pixels,
    /// Line numbers to draw: (top in document coordinates, line height, number).
    pub line_numbers: Vec<(Pixels, Pixels, u32)>,
}

impl DocLayout {
    pub fn empty() -> Self {
        Self {
            width: px(0.),
            scale_factor: 1.,
            decorations: Decorations {
                spelling: Vec::new(),
                marked: None,
            },
            dark: false,
            settings: EditorSettings::default(),
            source: Vec::new(),
            blocks: Vec::new(),
            height: px(0.),
            content_width: px(0.),
            gutter: px(0.),
            line_numbers: Vec::new(),
        }
    }

    /// Brings the layout up to date with `doc`, re-shaping only what changed.
    pub fn update(
        &mut self,
        doc: &Document,
        width: Pixels,
        settings: EditorSettings,
        decorations: &Decorations,
        theme: &Theme,
        window: &mut Window,
    ) {
        let scale_factor = window.scale_factor();
        let everything = width != self.width
            || (scale_factor - self.scale_factor).abs() > f32::EPSILON
            || settings != self.settings
            || theme.dark != self.dark
            || self.blocks.is_empty();
        let gutter = if settings.line_numbers {
            px(LINE_NUMBER_GUTTER)
        } else {
            px(0.)
        };
        let text_width = (width - gutter).max(px(80.));
        let diff = if everything {
            inkveil_doc::BlockDiff {
                start: 0,
                old_end: self.source.len(),
                new_end: doc.len(),
            }
        } else {
            diff_blocks(&self.source, doc.blocks())
        };
        let decorated = if everything || *decorations == self.decorations {
            Vec::new()
        } else {
            decoration_changes(&self.decorations, decorations)
        };
        if diff.is_empty() && decorated.is_empty() {
            return;
        }
        let fresh: Vec<BlockLayout> = (diff.start..diff.new_end)
            .map(|i| {
                let mut block =
                    layout_block(doc, i, text_width, settings, decorations, theme, window);
                block.left += gutter;
                block
            })
            .collect();
        self.blocks.splice(diff.start..diff.old_end, fresh);
        for i in decorated {
            if i < doc.len() && !(diff.start..diff.new_end).contains(&i) {
                let mut block =
                    layout_block(doc, i, text_width, settings, decorations, theme, window);
                block.left += gutter;
                self.blocks[i] = block;
            }
        }
        self.source = doc.blocks().to_vec();
        self.width = width;
        self.scale_factor = scale_factor;
        self.settings = settings;
        self.gutter = gutter;
        self.dark = theme.dark;
        self.decorations = decorations.clone();
        self.refresh_markers(doc);

        let mut y = px(0.);
        for (i, block) in self.blocks.iter_mut().enumerate() {
            if i > 0 {
                y += block.margin_top;
            }
            block.top = y;
            y += block.height;
        }
        self.height = y;
        self.measure_lines();
    }

    /// Recomputes content width and line numbers from the positioned blocks.
    fn measure_lines(&mut self) {
        let body_line = px(BASE_FONT_SIZE * LINE_HEIGHT).round();
        let mut widest = px(0.);
        let mut numbers = Vec::new();
        let mut number = 0u32;
        for block in &self.blocks {
            match &block.content {
                BlockContent::Text { text, .. } => {
                    widest = widest.max(block.left + text.origin.x + text.max_row_width());
                    for top in text.line_tops() {
                        number += 1;
                        numbers.push((block.top + text.origin.y + top, text.line_height, number));
                    }
                }
                BlockContent::Table(table) => {
                    widest = widest.max(block.left + table.width);
                    number += 1;
                    numbers.push((block.top + px(CELL_PAD_Y), body_line, number));
                }
                BlockContent::Divider => {
                    number += 1;
                    numbers.push((
                        block.top + (block.height - body_line) / 2.,
                        body_line,
                        number,
                    ));
                }
                BlockContent::Image { .. } => {
                    number += 1;
                    numbers.push((block.top, body_line, number));
                }
            }
        }
        self.content_width = widest;
        self.line_numbers = numbers;
    }

    /// Recomputes list numbers, which depend on neighbouring blocks.
    fn refresh_markers(&mut self, doc: &Document) {
        let numbers = list_numbers(doc);
        for (block, number) in self.blocks.iter_mut().zip(numbers) {
            if let (
                BlockContent::Text {
                    marker: Marker::Number(n),
                    ..
                },
                Some(number),
            ) = (&mut block.content, number)
            {
                *n = number;
            }
        }
    }

    /// Index of the block at document y (clamped to the nearest block).
    pub fn block_at_y(&self, y: Pixels) -> usize {
        self.blocks
            .partition_point(|b| b.top + b.height < y)
            .min(self.blocks.len().saturating_sub(1))
    }

    /// Caret rectangle (top-left, height) for `pos`, in document coordinates.
    pub fn caret(&self, pos: Pos) -> Option<(Point<Pixels>, Pixels)> {
        let block = self.blocks.get(pos.loc.block)?;
        let (text, origin) = block.text_at(pos.loc.cell)?;
        let p = text.position_for_offset(pos.offset);
        Some((
            point(origin.x + p.x, block.top + origin.y + p.y),
            text.line_height,
        ))
    }

    /// The caret position closest to a document point.
    pub fn hit(&self, doc: &Document, position: Point<Pixels>) -> Pos {
        if self.blocks.is_empty() {
            return doc.start();
        }
        let index = self.block_at_y(position.y);
        let block = &self.blocks[index];
        let local = point(position.x - block.left, position.y - block.top);
        match &block.content {
            BlockContent::Text { text, .. } => {
                Pos::new(index, text.offset_for_position(local - text.origin))
            }
            BlockContent::Table(table) => {
                let cell = table.cell_at(local);
                let offset = table
                    .cells
                    .get(usize::from(cell.row))
                    .and_then(|r| r.get(usize::from(cell.col)))
                    .map_or(0, |t| {
                        t.offset_for_position(local - table.cell_origin(cell))
                    });
                Pos::in_cell(index, cell.row, cell.col, offset)
            }
            BlockContent::Divider | BlockContent::Image { .. } => doc.clamp(Pos::new(index, 0)),
        }
    }

    /// The picture under a document point: its block index and bounds (document coordinates).
    pub fn image_at(&self, position: Point<Pixels>) -> Option<(usize, Bounds<Pixels>)> {
        let index = self.block_at_y(position.y);
        let block = self.blocks.get(index)?;
        let BlockContent::Image { size, x, .. } = &block.content else {
            return None;
        };
        let bounds = Bounds::new(point(block.left + *x, block.top), *size);
        bounds.contains(&position).then_some((index, bounds))
    }
}

fn layout_block(
    doc: &Document,
    index: usize,
    width: Pixels,
    settings: EditorSettings,
    decorations: &Decorations,
    theme: &Theme,
    window: &mut Window,
) -> BlockLayout {
    let block = doc.block(index).expect("index within document");
    match block {
        Block::Text(tb) => {
            let style = block_text_style(tb, theme);
            let left = px(INDENT_STEP * f32::from(tb.indent));
            let gutter = if tb.kind.is_list() {
                px(LIST_GUTTER)
            } else if tb.kind == TextKind::Quote {
                px(QUOTE_GUTTER)
            } else {
                px(0.)
            };
            let padding = if style.code { px(CODE_PADDING) } else { px(0.) };
            let wrap_width = (width - left - gutter - padding * 2.).max(px(40.));
            let mut text = TextLayout::shape(
                &tb.text,
                &style,
                wrap_width,
                settings.wrap,
                Loc::block(index),
                decorations,
                theme,
                window,
            );
            text.origin = point(gutter + padding, padding);
            let marker = match tb.kind {
                TextKind::Bullet => Marker::Bullet(tb.indent),
                TextKind::Numbered => Marker::Number(String::new()),
                TextKind::Task { checked } => Marker::Checkbox(checked),
                _ => Marker::None,
            };
            let margin_top = match tb.kind {
                TextKind::Heading(_) => px(HEADING_GAP),
                _ => px(BLOCK_GAP),
            };
            BlockLayout {
                margin_top,
                height: text.height() + padding * 2.,
                left,
                content: BlockContent::Text {
                    text,
                    marker,
                    quote: tb.kind == TextKind::Quote,
                    code: style.code,
                },
                top: px(0.),
            }
        }
        Block::Divider => BlockLayout {
            margin_top: px(BLOCK_GAP),
            height: px(DIVIDER_HEIGHT),
            left: px(0.),
            content: BlockContent::Divider,
            top: px(0.),
        },
        Block::Image(image) => {
            // Never wider than the picture's own pixels at this display's scale.
            let natural = px(image.pixel_width as f32 / window.scale_factor().max(1.));
            let width_px = (width * (f32::from(image.width_percent) / 100.))
                .min(natural)
                .max(px(24.));
            let height = width_px * (image.pixel_height as f32 / image.pixel_width as f32);
            let x = match image.align {
                inkveil_doc::Align::Left => px(0.),
                inkveil_doc::Align::Center => ((width - width_px) / 2.).max(px(0.)),
                inkveil_doc::Align::Right => (width - width_px).max(px(0.)),
            };
            BlockLayout {
                margin_top: px(BLOCK_GAP * 2.),
                height,
                left: px(0.),
                content: BlockContent::Image {
                    blob: image.blob,
                    size: size(width_px, height),
                    x,
                    column: width,
                },
                top: px(0.),
            }
        }
        Block::Table(table) => {
            let cols = usize::from(table.col_count());
            let col_width = (width / cols as f32).max(px(MIN_COL_WIDTH));
            let style = cell_text_style(theme);
            let mut cells = Vec::new();
            let mut row_heights = Vec::new();
            for (r, row) in table.rows().iter().enumerate() {
                let mut laid = Vec::new();
                let mut height = px(0.);
                for (c, cell) in row.iter().enumerate() {
                    let loc = Loc::cell(
                        index,
                        u16::try_from(r).unwrap_or(0),
                        u16::try_from(c).unwrap_or(0),
                    );
                    let text = TextLayout::shape(
                        cell,
                        &style,
                        (col_width - px(CELL_PAD_X * 2.)).max(px(20.)),
                        true,
                        loc,
                        decorations,
                        theme,
                        window,
                    );
                    height = height.max(text.height());
                    laid.push(text);
                }
                row_heights.push(height + px(CELL_PAD_Y * 2.));
                cells.push(laid);
            }
            let col_x = (0..cols).map(|c| col_width * c as f32).collect();
            let mut row_y = Vec::with_capacity(row_heights.len());
            let mut y = px(0.);
            for h in &row_heights {
                row_y.push(y);
                y += *h;
            }
            let layout = TableLayout {
                col_x,
                row_y,
                row_heights,
                width: col_width * cols as f32,
                cells,
            };
            BlockLayout {
                margin_top: px(BLOCK_GAP * 2.),
                height: layout.height(),
                left: px(0.),
                content: BlockContent::Table(layout),
                top: px(0.),
            }
        }
    }
}

/// Blocks whose decorations differ between `old` and `new`.
fn decoration_changes(old: &Decorations, new: &Decorations) -> Vec<usize> {
    let mut blocks: Vec<usize> = old
        .spelling
        .iter()
        .filter(|d| !new.spelling.contains(d))
        .chain(new.spelling.iter().filter(|d| !old.spelling.contains(d)))
        .map(|(loc, _)| loc.block)
        .collect();
    if old.marked != new.marked {
        blocks.extend(
            old.marked
                .iter()
                .chain(&new.marked)
                .map(|(loc, _)| loc.block),
        );
    }
    blocks.sort_unstable();
    blocks.dedup();
    blocks
}

/// The number shown for each numbered list item (`None` for other blocks).
///
/// Numbering continues across consecutive numbered items at the same indent, through deeper
/// nested items, and restarts after anything else.
fn list_numbers(doc: &Document) -> Vec<Option<String>> {
    let mut counters: Vec<u32> = Vec::new();
    doc.blocks()
        .iter()
        .map(|block| match block.as_text() {
            Some(t) if t.kind == TextKind::Numbered => {
                let level = usize::from(t.indent);
                counters.truncate(level + 1);
                while counters.len() <= level {
                    counters.push(0);
                }
                counters[level] += 1;
                Some(format!("{}.", counters[level]))
            }
            Some(t) if t.kind.is_list() => {
                counters.truncate(usize::from(t.indent));
                None
            }
            _ => {
                counters.clear();
                None
            }
        })
        .collect()
}
