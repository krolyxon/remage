//! Grid layout, cursor, scrolling and the visual-mode range.
//!
//! All sizes are physical pixels. Content coordinates start at the top of the first row's
//! gap; the scroll offset is how far the content has moved up.

use std::ops::Range;
use std::time::{Duration, Instant};

use super::view::Dir;
use crate::anim::Tween;

/// One frame's grid geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub count: usize,
    pub cols: usize,
    /// Side of the square each thumbnail fits in.
    pub cell: f32,
    pub gap: f32,
    /// Room under each cell for its name (0 without names).
    pub label: f32,
    /// x of the first column; the grid is centred.
    pub left: f32,
    pub viewport: [f32; 2],
}

impl Layout {
    pub fn new(count: usize, viewport: [f32; 2], cell: f32, gap: f32, label: f32) -> Self {
        let cell = cell.max(1.0);
        let cols = ((viewport[0] - gap) / (cell + gap)).floor().max(1.0) as usize;
        let used = cols as f32 * cell + (cols - 1) as f32 * gap;
        Self {
            count,
            cols,
            cell,
            gap,
            label,
            left: ((viewport[0] - used) * 0.5).max(0.0).round(),
            viewport,
        }
    }

    pub fn row_height(&self) -> f32 {
        self.cell + self.label + self.gap
    }

    pub fn rows(&self) -> usize {
        self.count.div_ceil(self.cols)
    }

    pub fn row_of(&self, i: usize) -> usize {
        i / self.cols
    }

    pub fn content_height(&self) -> f32 {
        self.gap + self.rows() as f32 * self.row_height()
    }

    pub fn max_scroll(&self) -> f32 {
        (self.content_height() - self.viewport[1]).max(0.0)
    }

    pub fn clamp_scroll(&self, s: f32) -> f32 {
        s.clamp(0.0, self.max_scroll())
    }

    /// Top-left of cell `i` in content coordinates.
    pub fn origin(&self, i: usize) -> [f32; 2] {
        let (row, col) = (i / self.cols, i % self.cols);
        [
            self.left + col as f32 * (self.cell + self.gap),
            self.gap + row as f32 * self.row_height(),
        ]
    }

    /// Cells at least partly on screen at `scroll`.
    pub fn visible(&self, scroll: f32) -> Range<usize> {
        self.rows_between(scroll, scroll + self.viewport[1])
    }

    /// Cells whose rows overlap content `top..bottom`.
    pub fn rows_between(&self, top: f32, bottom: f32) -> Range<usize> {
        let rh = self.row_height();
        let first = ((top - self.gap - self.cell - self.label) / rh)
            .ceil()
            .max(0.0) as usize;
        let last = ((bottom - self.gap) / rh).ceil().max(0.0) as usize;
        let start = (first * self.cols).min(self.count);
        let end = (last * self.cols).min(self.count);
        start..end.max(start)
    }

    /// Whole rows that fit on screen (at least one).
    pub fn page_rows(&self) -> usize {
        ((self.viewport[1] - self.gap) / self.row_height())
            .floor()
            .max(1.0) as usize
    }

    /// The cell under a viewport point.
    pub fn hit(&self, p: [f32; 2], scroll: f32) -> Option<usize> {
        let y = p[1] + scroll - self.gap;
        let x = p[0] - self.left;
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let (col, row) = (
            (x / (self.cell + self.gap)) as usize,
            (y / self.row_height()) as usize,
        );
        let in_cell = x - col as f32 * (self.cell + self.gap) <= self.cell
            && y - row as f32 * self.row_height() <= self.cell + self.label;
        let i = row * self.cols + col;
        (col < self.cols && in_cell && i < self.count).then_some(i)
    }

    /// The scroll offset nearest to `scroll` that shows cell `i`'s row whole, gaps included.
    pub fn follow(&self, i: usize, scroll: f32) -> f32 {
        let top = self.row_of(i) as f32 * self.row_height();
        let bottom = top + self.row_height() + self.gap;
        let s = if top < scroll {
            top
        } else if bottom > scroll + self.viewport[1] {
            bottom - self.viewport[1]
        } else {
            scroll
        };
        self.clamp_scroll(s)
    }

    /// Move the cursor `n` cells (left/right) or rows (up/down). Down onto a short last row
    /// lands on the last cell.
    pub fn step(&self, i: usize, dir: Dir, n: usize) -> usize {
        if self.count == 0 {
            return 0;
        }
        let last = self.count - 1;
        match dir {
            Dir::Left => i.saturating_sub(n),
            Dir::Right => i.saturating_add(n).min(last),
            Dir::Up => i - n.min(self.row_of(i)) * self.cols,
            Dir::Down => {
                let rows = n.min(self.row_of(last) - self.row_of(i));
                (i + rows * self.cols).min(last)
            }
        }
    }

    /// Move by whole rows, clamping into the first or last row instead of stopping.
    pub fn step_rows(&self, i: usize, rows: isize) -> usize {
        if self.count == 0 {
            return 0;
        }
        let target = i as isize + rows * self.cols as isize;
        target.clamp(0, self.count as isize - 1) as usize
    }

    pub fn row_start(&self, i: usize) -> usize {
        i - i % self.cols
    }

    pub fn row_end(&self, i: usize) -> usize {
        (self.row_start(i) + self.cols - 1).min(self.count.saturating_sub(1))
    }

    /// Bring the cursor onto the screen at `scroll`, keeping its column.
    pub fn pull_into_view(&self, i: usize, scroll: f32) -> usize {
        if self.count == 0 {
            return 0;
        }
        let rh = self.row_height();
        let row = self.row_of(i) as f32;
        // First and last rows fully on screen.
        let first = (scroll / rh).ceil();
        let last = ((scroll + self.viewport[1] - self.gap) / rh).floor() - 1.0;
        let target = if row < first {
            first
        } else if row > last && last >= first {
            last
        } else {
            return i;
        };
        let j = target as usize * self.cols + i % self.cols;
        j.min(self.count - 1)
    }
}

/// Grid view state that outlives a frame: size, scroll and the visual anchor.
#[derive(Clone, Debug)]
pub struct Grid {
    /// Logical thumbnail size, one of `grid.sizes`.
    pub size: u32,
    scroll: Tween<f32>,
    /// Visual mode's fixed end.
    pub anchor: Option<usize>,
}

impl Grid {
    pub fn new(size: u32, now: Instant) -> Self {
        Self {
            size,
            scroll: Tween::new(0.0, now),
            anchor: None,
        }
    }

    /// The scroll offset to draw at `now`.
    pub fn scroll(&self, layout: &Layout, now: Instant) -> f32 {
        layout.clamp_scroll(self.scroll.value(now))
    }

    /// Where scrolling is heading.
    pub fn target(&self, layout: &Layout) -> f32 {
        layout.clamp_scroll(self.scroll.target())
    }

    pub fn scroll_to(&mut self, to: f32, now: Instant, dur: Duration) {
        if (self.scroll.target() - to).abs() > 0.01 {
            self.scroll.retarget(to, now, dur);
        }
    }

    pub fn jump_to(&mut self, to: f32) {
        self.scroll.set(to);
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.scroll.is_active(now)
    }

    /// The visual range, inclusive, given the cursor.
    pub fn range(&self, cursor: usize) -> Option<Range<usize>> {
        self.anchor.map(|a| a.min(cursor)..a.max(cursor) + 1)
    }
}

/// Step through `sizes` from the entry nearest `current`.
pub fn next_size(sizes: &[u32], current: u32, delta: i32) -> u32 {
    if sizes.is_empty() {
        return current;
    }
    let i = sizes
        .iter()
        .enumerate()
        .min_by_key(|(_, s)| s.abs_diff(current))
        .map_or(0, |(i, _)| i);
    let j = (i as i64 + delta as i64).clamp(0, sizes.len() as i64 - 1) as usize;
    sizes[j]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 cells of 100 with gap 10 in 450×300: 4 columns, 3 rows.
    fn layout(count: usize) -> Layout {
        Layout::new(count, [450.0, 300.0], 100.0, 10.0, 0.0)
    }

    #[test]
    fn columns_are_centred() {
        let l = layout(10);
        assert_eq!(l.cols, 4);
        assert_eq!(l.left, 10.0);
        assert_eq!(l.rows(), 3);
        assert_eq!(l.origin(5), [120.0, 120.0]);
        assert_eq!(l.content_height(), 340.0);
        assert_eq!(l.max_scroll(), 40.0);
        // Never fewer than one column.
        assert_eq!(Layout::new(3, [50.0, 50.0], 100.0, 10.0, 0.0).cols, 1);
    }

    #[test]
    fn cursor_moves_like_nsxiv() {
        let l = layout(10);
        assert_eq!(l.step(0, Dir::Left, 1), 0);
        assert_eq!(l.step(4, Dir::Left, 1), 3);
        assert_eq!(l.step(3, Dir::Right, 1), 4);
        assert_eq!(l.step(9, Dir::Right, 5), 9);
        assert_eq!(l.step(5, Dir::Up, 1), 1);
        assert_eq!(l.step(1, Dir::Up, 1), 1);
        assert_eq!(l.step(5, Dir::Up, 9), 1);
        assert_eq!(l.step(1, Dir::Down, 1), 5);
        // Onto the short last row: the last cell.
        assert_eq!(l.step(7, Dir::Down, 1), 9);
        assert_eq!(l.step(9, Dir::Down, 1), 9);
        assert_eq!(l.step(2, Dir::Down, 5), 9);
        assert_eq!(l.row_start(6), 4);
        assert_eq!(l.row_end(6), 7);
        assert_eq!(l.row_end(8), 9);
        assert_eq!(l.step_rows(2, 5), 9);
        assert_eq!(l.step_rows(6, -5), 0);
        assert_eq!(layout(0).step(0, Dir::Down, 1), 0);
    }

    #[test]
    fn visibility_and_following() {
        let l = layout(40); // 10 rows, content 1110 high
        assert_eq!(l.visible(0.0), 0..12);
        assert_eq!(l.visible(110.0), 0..16);
        assert_eq!(l.visible(115.0), 4..16);
        // Row 5 spans 550..670 including the gaps; a 300 viewport must scroll to 370.
        assert_eq!(l.follow(20, 0.0), 370.0);
        assert_eq!(l.follow(20, 400.0), 400.0);
        assert_eq!(l.follow(0, 400.0), 0.0);
        assert_eq!(l.page_rows(), 2);
    }

    #[test]
    fn hit_testing() {
        let l = layout(10);
        assert_eq!(l.hit([15.0, 15.0], 0.0), Some(0));
        assert_eq!(l.hit([125.0, 125.0], 0.0), Some(5));
        // In a gap.
        assert_eq!(l.hit([115.0, 15.0], 0.0), None);
        // Past the last cell.
        assert_eq!(l.hit([345.0, 235.0], 0.0), None);
        assert_eq!(l.hit([15.0, 15.0], 110.0), Some(4));
    }

    #[test]
    fn pulling_the_cursor_into_view() {
        let l = layout(40);
        assert_eq!(l.pull_into_view(1, 0.0), 1);
        // Scrolled so rows 2 and 3 are whole: row 0 comes down to row 2.
        assert_eq!(l.pull_into_view(1, 220.0), 9);
        assert_eq!(l.pull_into_view(37, 220.0), 13);
    }

    #[test]
    fn sizes_step_and_clamp() {
        let s = [96, 128, 160, 224, 320];
        assert_eq!(next_size(&s, 160, 1), 224);
        assert_eq!(next_size(&s, 160, -2), 96);
        assert_eq!(next_size(&s, 320, 1), 320);
        assert_eq!(next_size(&s, 150, 0), 160);
    }

    #[test]
    fn visual_range_is_inclusive() {
        let mut g = Grid::new(160, Instant::now());
        assert_eq!(g.range(3), None);
        g.anchor = Some(5);
        assert_eq!(g.range(2), Some(2..6));
        assert_eq!(g.range(7), Some(5..8));
    }
}
