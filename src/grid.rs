use crate::term::BufWrite as _;

#[derive(Clone, Debug)]
pub struct Grid {
    size: Size,
    pos: Pos,
    saved_pos: Pos,
    rows: Vec<crate::row::Row>,
    scroll_top: u16,
    scroll_bottom: u16,
    origin_mode: bool,
    saved_origin_mode: bool,
    scrollback: std::collections::VecDeque<crate::row::Row>,
    scrollback_len: usize,
    scrollback_offset: usize,
}

impl Grid {
    pub fn new(size: Size, scrollback_len: usize) -> Self {
        Self {
            size,
            pos: Pos::default(),
            saved_pos: Pos::default(),
            rows: vec![],
            scroll_top: 0,
            scroll_bottom: size.rows - 1,
            origin_mode: false,
            saved_origin_mode: false,
            scrollback: std::collections::VecDeque::new(),
            scrollback_len,
            scrollback_offset: 0,
        }
    }

    pub fn allocate_rows(&mut self) {
        if self.rows.is_empty() {
            self.rows.extend(
                std::iter::repeat_with(|| {
                    crate::row::Row::new(self.size.cols)
                })
                .take(usize::from(self.size.rows)),
            );
        }
    }

    fn new_row(&self) -> crate::row::Row {
        crate::row::Row::new(self.size.cols)
    }

    pub fn clear(&mut self) {
        self.pos = Pos::default();
        self.saved_pos = Pos::default();
        for row in self.drawing_rows_mut() {
            row.clear(crate::attrs::Attrs::default());
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows - 1;
        self.origin_mode = false;
        self.saved_origin_mode = false;
    }

    pub fn size(&self) -> Size {
        self.size
    }

    /// Resizes the grid. When `reflow` is true and the number of columns
    /// changes, soft-wrapped lines in the scrollback and visible area are
    /// re-chunked to the new width so no content is truncated. The legacy
    /// truncate/pad behavior is used for the alternate screen grid (which is
    /// laid out by absolute cursor addressing and repainted on SIGWINCH).
    pub fn set_size(&mut self, size: Size, reflow: bool) {
        if reflow && size.cols != self.size.cols {
            self.reflow(size);
            return;
        }

        let old_rows = self.size.rows;
        let new_rows = size.rows;

        if self.scroll_bottom == self.size.rows - 1 {
            self.scroll_bottom = size.rows - 1;
        }

        self.size = size;

        // History pull-down on vertical grow: reveal the most recent scrollback
        // rows at the top of the grid instead of padding blank rows at the
        // bottom, keeping the cursor/prompt bottom-anchored (mirrors the shrink
        // path's ESC[S scroll-up). Only while following the tail AND the cursor
        // rests on the bottom edge — pulling otherwise would shove a top-anchored
        // prompt down into blanks and, across grow/shrink oscillations, multiply
        // blank lines into the scrollback. The alternate screen auto-bypasses
        // because its scrollback is always empty.
        if new_rows > old_rows
            && self.scrollback_offset == 0
            && !self.scrollback.is_empty()
            && self.pos.row >= old_rows.saturating_sub(1)
        {
            let pull =
                usize::from(new_rows - old_rows).min(self.scrollback.len());
            // split_off preserves chronological order (the tail of the scrollback
            // is the most recent history, adjacent to the old grid's first row).
            // A `pop_back` loop would yield newest→oldest and display the
            // revealed history upside-down — do NOT use it.
            let split_at = self.scrollback.len() - pull;
            let pulled: Vec<_> =
                self.scrollback.split_off(split_at).into_iter().collect();
            self.rows.splice(0..0, pulled);
            // pull <= new_rows - old_rows, both u16, so this never truncates.
            let pull_u16 = u16::try_from(pull).unwrap_or(u16::MAX);
            self.pos.row = self.pos.row.saturating_add(pull_u16);
            self.saved_pos.row = self.saved_pos.row.saturating_add(pull_u16);
        }

        for row in &mut self.rows {
            row.resize(size.cols, crate::Cell::new());
        }
        self.rows.resize(usize::from(size.rows), self.new_row());

        if self.scroll_bottom >= size.rows {
            self.scroll_bottom = size.rows - 1;
        }
        if self.scroll_bottom < self.scroll_top {
            self.scroll_top = 0;
        }

        self.row_clamp_top(false);
        self.row_clamp_bottom(false);
        self.col_clamp();

        if self.saved_pos.row > self.size.rows - 1 {
            self.saved_pos.row = self.size.rows - 1;
        }
        if self.saved_pos.col > self.size.cols - 1 {
            self.saved_pos.col = self.size.cols - 1;
        }
    }

    /// Reflows all content (scrollback + visible rows) to a new size when the
    /// width changes, then remaps the cursor, saved cursor position, and the
    /// scrollback viewport anchor.
    fn reflow(&mut self, new_size: Size) {
        let old_cols = self.size.cols;
        let old_rows = self.size.rows;
        let new_cols = new_size.cols;
        let new_rows = usize::from(new_size.rows);
        let sb_cap = self.scrollback_len;

        let old_pos = self.pos;
        let old_saved_pos = self.saved_pos;
        let old_scrollback_offset = self.scrollback_offset;
        let old_sb_len = self.scrollback.len();

        self.size = new_size;

        // Move the grid out so we can read it while rebuilding.
        let sb_rows = std::mem::take(&mut self.scrollback);
        let vis_rows = std::mem::take(&mut self.rows);
        let all_len = sb_rows.len() + vis_rows.len();

        // Anchors to remap through the reflow. The cursor anchors the
        // reassembly: when following the tail, rows below the cursor line are
        // dropped as filler and the oldest rows above are evicted to fit the
        // new height, keeping the cursor line visible. Rows without content
        // map to their reflowed line index (equivalent to the legacy clamped
        // position on empty screens); rows with content are mapped through the
        // reflow so the cursor follows its cell.
        let cursor_g = (old_sb_len + usize::from(old_pos.row))
            .min(all_len.saturating_sub(1));
        let cursor_col = old_pos.col;
        let cursor_pending = old_pos.col == old_cols;
        let cursor_row_blank =
            grid_row_is_blank(grid_row_at(&sb_rows, &vis_rows, cursor_g));
        let saved_g = (old_sb_len + usize::from(old_saved_pos.row))
            .min(all_len.saturating_sub(1));
        let saved_col = old_saved_pos.col;
        let saved_row_blank =
            grid_row_is_blank(grid_row_at(&sb_rows, &vis_rows, saved_g));
        let viewport_anchor = if old_scrollback_offset > 0 {
            Some(
                old_sb_len
                    .saturating_sub(old_scrollback_offset)
                    .min(all_len.saturating_sub(1)),
            )
        } else {
            None
        };

        // Group rows into logical lines: a row continues the previous line
        // when it is soft-wrapped, or when a wide character was pushed to the
        // next row leaving a blank last column (vt100 does not set the wrap
        // flag in that case).
        let mut line_starts: Vec<usize> = Vec::new();
        {
            let mut prev_last_empty = false;
            for g in 0..all_len {
                if g == 0 {
                    line_starts.push(0);
                } else {
                    let continuation = {
                        let prev = grid_row_at(&sb_rows, &vis_rows, g - 1);
                        let cur = grid_row_at(&sb_rows, &vis_rows, g);
                        prev.wrapped()
                            || (prev_last_empty
                                && cur
                                    .get(0)
                                    .is_some_and(crate::Cell::is_wide))
                    };
                    if !continuation {
                        line_starts.push(g);
                    }
                }
                let row = grid_row_at(&sb_rows, &vis_rows, g);
                prev_last_empty = row
                    .get(row.cols().saturating_sub(1))
                    .is_some_and(|c| !c.has_contents());
            }
        }

        let mut all_new: Vec<crate::row::Row> = Vec::new();
        let mut cursor_res: Option<(usize, u16)> = None;
        let mut saved_res: Option<(usize, u16)> = None;
        let mut viewport_res: Option<(usize, u16)> = None;
        let mut cursor_line_end: Option<usize> = None;

        for (li, &start) in line_starts.iter().enumerate() {
            let end = line_starts.get(li + 1).copied().unwrap_or(all_len);
            let line_start_in_all = all_new.len();

            // Flatten the logical line into a stream of written cells. Wide
            // continuation cells are skipped and trailing unwritten default
            // padding is dropped, except for the cursor row which keeps blank
            // cells up to the cursor column so its position survives the
            // reflow. Cells without contents but with non-default attributes
            // (e.g. a background-color region painted with EL / \x1b[K) are
            // kept so background colors survive the reflow.
            let mut cells: Vec<crate::Cell> = Vec::new();
            let mut src: Vec<(usize, u16)> = Vec::new();
            for g in start..end {
                let row = grid_row_at(&sb_rows, &vis_rows, g);
                let cols = row.cols();
                let mut last_meaningful: u16 = 0;
                for c in 0..cols {
                    if let Some(ce) = row.get(c) {
                        if ce.has_contents()
                            || ce.attrs() != &crate::attrs::Attrs::default()
                        {
                            last_meaningful = c + 1;
                        }
                    }
                }
                let mut row_end = last_meaningful;
                if g == cursor_g
                    && !cursor_row_blank
                    && cursor_col < old_cols
                    && cursor_col + 1 > row_end
                {
                    row_end = cursor_col + 1;
                }
                if row_end > cols {
                    row_end = cols;
                }
                for c in 0..row_end {
                    if let Some(ce) = row.get(c) {
                        if ce.is_wide_continuation() {
                            continue;
                        }
                        cells.push(ce.clone());
                        src.push((g, c));
                    }
                }
            }

            // Re-wrap the stream at the new width.
            let (mut rws, widths) = Self::reflow_rows(&cells, new_cols);
            for rw in &mut rws {
                rw.pad(new_cols);
            }

            if cursor_g >= start && cursor_g < end {
                if cursor_row_blank {
                    cursor_res =
                        Some((line_start_in_all, cursor_col.min(new_cols)));
                } else {
                    let target = Self::reflow_forward(
                        &cells, &src, cursor_g, cursor_col,
                    );
                    let (lr, lc) =
                        Self::reflow_reverse(&widths, target, &rws, new_cols);
                    cursor_res = Some((line_start_in_all + lr, lc));
                }
                cursor_line_end = Some(line_start_in_all + rws.len());
            }
            if saved_g >= start && saved_g < end {
                if saved_row_blank {
                    saved_res =
                        Some((line_start_in_all, saved_col.min(new_cols)));
                } else {
                    let target = Self::reflow_forward(
                        &cells, &src, saved_g, saved_col,
                    );
                    let (lr, lc) =
                        Self::reflow_reverse(&widths, target, &rws, new_cols);
                    saved_res = Some((line_start_in_all + lr, lc));
                }
            }
            if let Some(vg) = viewport_anchor {
                if vg >= start && vg < end {
                    let target = Self::reflow_forward(&cells, &src, vg, 0);
                    let (lr, lc) =
                        Self::reflow_reverse(&widths, target, &rws, new_cols);
                    viewport_res = Some((line_start_in_all + lr, lc));
                }
            }

            all_new.extend(rws);
        }

        let cap_total = sb_cap + new_rows;

        // When following the tail, drop rows below the end of the cursor's
        // logical line (filler or content scrolled off by the height change) so
        // empty trailing rows never force real content to be evicted, while
        // keeping the cursor's own wrapped line intact.
        if old_scrollback_offset == 0 {
            if let Some(cle) = cursor_line_end {
                if cle < all_new.len() {
                    all_new.truncate(cle);
                }
            }
        }

        // Evict the oldest lines when the reflowed content exceeds capacity.
        let evict = all_new.len().saturating_sub(cap_total);
        if evict > 0 {
            all_new.drain(..evict);
        }

        let hold = all_new.len().saturating_sub(new_rows);

        // Resolve an anchor's post-eviction global row, or None if it was
        // evicted from the buffer entirely.
        let resolve = |pre: Option<(usize, u16)>| -> Option<(usize, u16)> {
            let (g, c) = pre?;
            if g >= evict && g - evict < all_new.len() {
                Some((g - evict, c))
            } else {
                None
            }
        };

        let new_offset = if old_scrollback_offset == 0 {
            // Tail-follow: stay at the bottom of the reflowed content.
            0
        } else {
            // Preserve the previously visible top line; fall back to the top
            // of the remaining scrollback if it was evicted.
            match resolve(viewport_res) {
                Some((vg, _)) => {
                    hold.saturating_sub(vg).min(hold).min(sb_cap)
                }
                None => hold.min(sb_cap),
            }
        };
        self.scrollback_offset = new_offset.min(hold).min(sb_cap);

        let first_visible = hold.saturating_sub(self.scrollback_offset);

        // Resolve the cursor to a drawing position. Blank rows map to their
        // line index (legacy clamp equivalent); content rows are anchored
        // through the reflow.
        let (cursor_draw_row, cursor_draw_col) = match resolve(cursor_res) {
            Some((cg, cc)) => {
                let draw = cg
                    .saturating_sub(first_visible)
                    .min(new_rows.saturating_sub(1));
                (draw, cc)
            }
            None => (new_rows.saturating_sub(1), new_cols.saturating_sub(1)),
        };

        if cursor_pending && !cursor_row_blank && cursor_draw_col == new_cols
        {
            self.pos = crate::grid::Pos {
                row: cursor_draw_row.try_into().unwrap(),
                col: new_cols,
            };
        } else {
            self.pos = crate::grid::Pos {
                row: cursor_draw_row.try_into().unwrap(),
                col: cursor_draw_col.min(new_cols.saturating_sub(1)),
            };
        }

        match resolve(saved_res) {
            Some((sg, sc)) => {
                let draw = sg
                    .saturating_sub(first_visible)
                    .min(new_rows.saturating_sub(1));
                self.saved_pos = crate::grid::Pos {
                    row: draw.try_into().unwrap(),
                    col: sc.min(new_cols.saturating_sub(1)),
                };
            }
            None => {
                // The saved position's line was dropped during reflow; fall
                // back to the legacy clamped position within the new bounds.
                self.saved_pos = crate::grid::Pos {
                    row: usize::from(old_saved_pos.row)
                        .min(new_rows.saturating_sub(1))
                        .try_into()
                        .unwrap(),
                    col: old_saved_pos.col.min(new_cols.saturating_sub(1)),
                };
            }
        }

        // Reassemble the scrollback and visible rows.
        self.scrollback = all_new[..hold].to_vec().into();
        let mut visible: Vec<crate::row::Row> = all_new[hold..].to_vec();

        // Bottom-anchor short content: when the screen grows TALLER and the
        // cursor was resting on a non-blank (prompt) row at the old bottom while
        // tail-following, keep the prompt at the new bottom by padding blanks
        // ABOVE instead of stranding it mid-screen with blanks below (the
        // shell's SIGWINCH redraw would draw the prompt at the new bottom,
        // leaving the old prompt stranded in whitespace). A cursor parked on an
        // empty row keeps the legacy clamp behavior; width-only reflows leave
        // content top-anchored.
        if new_size.rows > old_rows
            && old_scrollback_offset == 0
            && !cursor_row_blank
            && old_pos.row >= old_rows.saturating_sub(1)
            && visible.len() < new_rows
        {
            let pad = new_rows - visible.len();
            let mut padded = Vec::with_capacity(new_rows);
            padded.resize(pad, crate::row::Row::new(new_cols));
            padded.extend(visible);
            visible = padded;

            // Shift the resolved cursors down by exactly the rows added above
            // so they stay pinned to the text they were sitting on (do NOT
            // hardcode the absolute bottom row — line de-wrapping can leave the
            // cursor above the content end).
            let pad_u16 = u16::try_from(pad).unwrap_or(u16::MAX);
            self.pos.row = self.pos.row.saturating_add(pad_u16);
            self.saved_pos.row = self.saved_pos.row.saturating_add(pad_u16);
        }

        self.rows = visible;
        while self.rows.len() < new_rows {
            self.rows.push(crate::row::Row::new(new_cols));
        }

        // Keep the legacy scroll-region adjustment semantics on resize: the
        // bottom margin that reached the old bottom edge is extended to the new
        // height (compare against the old size, set before `self.size` was
        // overwritten).
        if self.scroll_bottom == old_rows.saturating_sub(1) {
            self.scroll_bottom = new_size.rows.saturating_sub(1);
        }
        if self.scroll_bottom >= new_size.rows {
            self.scroll_bottom = new_size.rows.saturating_sub(1);
        }
        if self.scroll_bottom < self.scroll_top {
            self.scroll_top = 0;
        }
    }

    /// Flattens a logical line's cells into rows of `new_cols` columns,
    /// returning the rows and each row's content width (in columns). Wide
    /// characters are never split across a row boundary.
    fn reflow_rows(
        cells: &[crate::Cell],
        new_cols: u16,
    ) -> (Vec<crate::row::Row>, Vec<u16>) {
        if cells.is_empty() {
            return (vec![crate::row::Row::new(new_cols)], vec![0]);
        }
        let mut rows: Vec<crate::row::Row> = Vec::new();
        let mut widths: Vec<u16> = Vec::new();
        let mut cur: Vec<crate::Cell> = Vec::new();
        let mut cur_w: u16 = 0;
        for (i, cell) in cells.iter().enumerate() {
            let w = if cell.is_wide() { 2 } else { 1 };
            if cur_w + w > new_cols && !cur.is_empty() {
                rows.push(crate::row::Row::from_cells(
                    std::mem::take(&mut cur),
                    true,
                ));
                widths.push(cur_w);
                cur_w = 0;
            }
            cur.push(cell.clone());
            cur_w += w;
            if cell.is_wide() {
                let mut cont = cell.clone();
                cont.clear(*cell.attrs());
                cont.set_wide_continuation(true);
                cur.push(cont);
            }
            if i + 1 == cells.len() {
                rows.push(crate::row::Row::from_cells(
                    std::mem::take(&mut cur),
                    false,
                ));
                widths.push(cur_w);
            }
        }
        (rows, widths)
    }

    /// Sums the stream width (wide = 2, standard = 1) of every cell strictly
    /// before the position `(target_row, target_col)` in reading order. The
    /// stream excludes wide-continuation cells and trailing padding, so a
    /// cursor resting on a stripped wrap-pad lands on the pushed wide
    /// character that follows it.
    fn reflow_forward(
        cells: &[crate::Cell],
        src: &[(usize, u16)],
        target_row: usize,
        target_col: u16,
    ) -> usize {
        let mut acc = 0usize;
        for (k, &(sg, sc)) in src.iter().enumerate() {
            if sg < target_row || (sg == target_row && sc < target_col) {
                acc += if cells[k].is_wide() { 2 } else { 1 };
            } else {
                break;
            }
        }
        acc
    }

    /// Maps a stream index back to a `(row, col)` in the re-wrapped rows,
    /// walking each row's actual content width (rows can be shorter than
    /// `new_cols` when a wide character early-wraps). Clamps to the leading
    /// cell of a wide character if the index lands on its continuation.
    fn reflow_reverse(
        widths: &[u16],
        target: usize,
        rws: &[crate::row::Row],
        new_cols: u16,
    ) -> (usize, u16) {
        let mut acc = 0usize;
        for (i, &w) in widths.iter().enumerate() {
            let w = usize::from(w);
            if target < acc + w {
                let mut col = u16::try_from(target - acc).unwrap();
                if col < new_cols
                    && rws[i]
                        .get(col)
                        .is_some_and(crate::Cell::is_wide_continuation)
                {
                    col = col.saturating_sub(1);
                }
                return (i, col);
            }
            acc += w;
        }
        let i = widths.len().saturating_sub(1);
        (i, widths[i])
    }

    pub fn pos(&self) -> Pos {
        self.pos
    }

    pub fn set_pos(&mut self, mut pos: Pos) {
        if self.origin_mode {
            pos.row = pos.row.saturating_add(self.scroll_top);
        }
        self.pos = pos;
        self.row_clamp_top(self.origin_mode);
        self.row_clamp_bottom(self.origin_mode);
        self.col_clamp();
    }

    pub fn save_cursor(&mut self) {
        self.saved_pos = self.pos;
        self.saved_origin_mode = self.origin_mode;
    }

    pub fn restore_cursor(&mut self) {
        self.pos = self.saved_pos;
        self.origin_mode = self.saved_origin_mode;
    }

    pub fn visible_rows(&self) -> impl Iterator<Item = &crate::row::Row> {
        let scrollback_len = self.scrollback.len();
        let rows_len = self.rows.len();
        self.scrollback
            .iter()
            .skip(scrollback_len - self.scrollback_offset)
            // when scrollback_offset > rows_len (e.g. rows = 3,
            // scrollback_len = 10, offset = 9) the skip(10 - 9)
            // will take 9 rows instead of 3. we need to set
            // the upper bound to rows_len (e.g. 3)
            .take(rows_len)
            // same for rows_len - scrollback_offset (e.g. 3 - 9).
            // it'll panic with overflow. we have to saturate the subtraction.
            .chain(
                self.rows
                    .iter()
                    .take(rows_len.saturating_sub(self.scrollback_offset)),
            )
    }

    pub fn drawing_rows(&self) -> impl Iterator<Item = &crate::row::Row> {
        self.rows.iter()
    }

    pub fn drawing_rows_mut(
        &mut self,
    ) -> impl Iterator<Item = &mut crate::row::Row> {
        self.rows.iter_mut()
    }

    pub fn visible_row(&self, row: u16) -> Option<&crate::row::Row> {
        self.visible_rows().nth(usize::from(row))
    }

    pub fn drawing_row(&self, row: u16) -> Option<&crate::row::Row> {
        self.drawing_rows().nth(usize::from(row))
    }

    pub fn drawing_row_mut(
        &mut self,
        row: u16,
    ) -> Option<&mut crate::row::Row> {
        self.drawing_rows_mut().nth(usize::from(row))
    }

    pub fn current_row_mut(&mut self) -> &mut crate::row::Row {
        self.drawing_row_mut(self.pos.row)
            // we assume self.pos.row is always valid
            .unwrap()
    }

    pub fn visible_cell(&self, pos: Pos) -> Option<&crate::Cell> {
        self.visible_row(pos.row).and_then(|r| r.get(pos.col))
    }

    pub fn drawing_cell(&self, pos: Pos) -> Option<&crate::Cell> {
        self.drawing_row(pos.row).and_then(|r| r.get(pos.col))
    }

    pub fn drawing_cell_mut(&mut self, pos: Pos) -> Option<&mut crate::Cell> {
        self.drawing_row_mut(pos.row)
            .and_then(|r| r.get_mut(pos.col))
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback_len
    }

    pub fn scrollback(&self) -> usize {
        self.scrollback_offset
    }

    pub fn set_scrollback(&mut self, rows: usize) {
        self.scrollback_offset = rows.min(self.scrollback.len());
    }

    pub fn write_contents(&self, contents: &mut String) {
        let mut wrapping = false;
        for row in self.visible_rows() {
            row.write_contents(contents, 0, self.size.cols, wrapping);
            if !row.wrapped() {
                contents.push('\n');
            }
            wrapping = row.wrapped();
        }

        while contents.ends_with('\n') {
            contents.truncate(contents.len() - 1);
        }
    }

    pub fn write_contents_formatted(
        &self,
        contents: &mut Vec<u8>,
    ) -> crate::attrs::Attrs {
        crate::term::ClearAttrs.write_buf(contents);
        crate::term::ClearScreen.write_buf(contents);

        let mut prev_attrs = crate::attrs::Attrs::default();
        let mut prev_pos = Pos::default();
        let mut wrapping = false;
        for (i, row) in self.visible_rows().enumerate() {
            // we limit the number of cols to a u16 (see Size), so
            // visible_rows() can never return more rows than will fit
            let i = i.try_into().unwrap();
            let (new_pos, new_attrs) = row.write_contents_formatted(
                contents,
                0,
                self.size.cols,
                i,
                wrapping,
                Some(prev_pos),
                Some(prev_attrs),
            );
            prev_pos = new_pos;
            prev_attrs = new_attrs;
            wrapping = row.wrapped();
        }

        self.write_cursor_position_formatted(
            contents,
            Some(prev_pos),
            Some(prev_attrs),
        );

        prev_attrs
    }

    pub fn write_contents_diff(
        &self,
        contents: &mut Vec<u8>,
        prev: &Self,
        mut prev_attrs: crate::attrs::Attrs,
    ) -> crate::attrs::Attrs {
        let mut prev_pos = prev.pos;
        let mut wrapping = false;
        let mut prev_wrapping = false;
        for (i, (row, prev_row)) in
            self.visible_rows().zip(prev.visible_rows()).enumerate()
        {
            // we limit the number of cols to a u16 (see Size), so
            // visible_rows() can never return more rows than will fit
            let i = i.try_into().unwrap();
            let (new_pos, new_attrs) = row.write_contents_diff(
                contents,
                prev_row,
                0,
                self.size.cols,
                i,
                wrapping,
                prev_wrapping,
                prev_pos,
                prev_attrs,
            );
            prev_pos = new_pos;
            prev_attrs = new_attrs;
            wrapping = row.wrapped();
            prev_wrapping = prev_row.wrapped();
        }

        self.write_cursor_position_formatted(
            contents,
            Some(prev_pos),
            Some(prev_attrs),
        );

        prev_attrs
    }

    pub fn write_cursor_position_formatted(
        &self,
        contents: &mut Vec<u8>,
        prev_pos: Option<Pos>,
        prev_attrs: Option<crate::attrs::Attrs>,
    ) {
        let prev_attrs = prev_attrs.unwrap_or_default();
        // writing a character to the last column of a row doesn't wrap the
        // cursor immediately - it waits until the next character is actually
        // drawn. it is only possible for the cursor to have this kind of
        // position after drawing a character though, so if we end in this
        // position, we need to redraw the character at the end of the row.
        if prev_pos != Some(self.pos) && self.pos.col >= self.size.cols {
            let mut pos = Pos {
                row: self.pos.row,
                col: self.size.cols - 1,
            };
            if self
                .drawing_cell(pos)
                // we assume self.pos.row is always valid, and self.size.cols
                // - 1 is always a valid column
                .unwrap()
                .is_wide_continuation()
            {
                pos.col = self.size.cols - 2;
            }
            let cell =
                // we assume self.pos.row is always valid, and self.size.cols
                // - 2 must be a valid column because self.size.cols - 1 is
                // always valid and we just checked that the cell at
                // self.size.cols - 1 is a wide continuation character, which
                // means that the first half of the wide character must be
                // before it
                self.drawing_cell(pos).unwrap();
            if cell.has_contents() {
                if let Some(prev_pos) = prev_pos {
                    crate::term::MoveFromTo::new(prev_pos, pos)
                        .write_buf(contents);
                } else {
                    crate::term::MoveTo::new(pos).write_buf(contents);
                }
                cell.attrs().write_escape_code_diff(contents, &prev_attrs);
                contents.extend(cell.contents().as_bytes());
                prev_attrs.write_escape_code_diff(contents, cell.attrs());
            } else {
                // if the cell doesn't have contents, we can't have gotten
                // here by drawing a character in the last column. this means
                // that as far as i'm aware, we have to have reached here from
                // a newline when we were already after the end of an earlier
                // row. in the case where we are already after the end of an
                // earlier row, we can just write a few newlines, otherwise we
                // also need to do the same as above to get ourselves to after
                // the end of a row.
                let mut found = false;
                for i in (0..self.pos.row).rev() {
                    pos.row = i;
                    pos.col = self.size.cols - 1;
                    if self
                        .drawing_cell(pos)
                        // i is always less than self.pos.row, which we assume
                        // to be always valid, so it must also be valid.
                        // self.size.cols - 1 is always a valid col.
                        .unwrap()
                        .is_wide_continuation()
                    {
                        pos.col = self.size.cols - 2;
                    }
                    let cell = self
                        .drawing_cell(pos)
                        // i is always less than self.pos.row, which we assume
                        // to be always valid, so it must also be valid.
                        // self.size.cols - 2 is valid because self.size.cols
                        // - 1 is always valid, and col gets set to
                        // self.size.cols - 2 when the cell at self.size.cols
                        // - 1 is a wide continuation character, meaning that
                        // the first half of the wide character must be before
                        // it
                        .unwrap();
                    if cell.has_contents() {
                        if let Some(prev_pos) = prev_pos {
                            if prev_pos.row != i
                                || prev_pos.col < self.size.cols
                            {
                                crate::term::MoveFromTo::new(prev_pos, pos)
                                    .write_buf(contents);
                                cell.attrs().write_escape_code_diff(
                                    contents,
                                    &prev_attrs,
                                );
                                contents.extend(cell.contents().as_bytes());
                                prev_attrs.write_escape_code_diff(
                                    contents,
                                    cell.attrs(),
                                );
                            }
                        } else {
                            crate::term::MoveTo::new(pos).write_buf(contents);
                            cell.attrs().write_escape_code_diff(
                                contents,
                                &prev_attrs,
                            );
                            contents.extend(cell.contents().as_bytes());
                            prev_attrs.write_escape_code_diff(
                                contents,
                                cell.attrs(),
                            );
                        }
                        contents.extend(
                            "\n".repeat(usize::from(self.pos.row - i))
                                .as_bytes(),
                        );
                        found = true;
                        break;
                    }
                }

                // this can happen if you get the cursor off the end of a row,
                // and then do something to clear the end of the current row
                // without moving the cursor (IL, DL, ED, EL, etc). we know
                // there can't be something in the last column because we
                // would have caught that above, so it should be safe to
                // overwrite it.
                if !found {
                    pos = Pos {
                        row: self.pos.row,
                        col: self.size.cols - 1,
                    };
                    if let Some(prev_pos) = prev_pos {
                        crate::term::MoveFromTo::new(prev_pos, pos)
                            .write_buf(contents);
                    } else {
                        crate::term::MoveTo::new(pos).write_buf(contents);
                    }
                    contents.push(b' ');
                    // we know that the cell has no contents, but it still may
                    // have drawing attributes (background color, etc)
                    let end_cell = self
                        .drawing_cell(pos)
                        // we assume self.pos.row is always valid, and
                        // self.size.cols - 1 is always a valid column
                        .unwrap();
                    end_cell
                        .attrs()
                        .write_escape_code_diff(contents, &prev_attrs);
                    crate::term::SaveCursor.write_buf(contents);
                    crate::term::Backspace.write_buf(contents);
                    crate::term::EraseChar::new(1).write_buf(contents);
                    crate::term::RestoreCursor.write_buf(contents);
                    prev_attrs
                        .write_escape_code_diff(contents, end_cell.attrs());
                }
            }
        } else if let Some(prev_pos) = prev_pos {
            crate::term::MoveFromTo::new(prev_pos, self.pos)
                .write_buf(contents);
        } else {
            crate::term::MoveTo::new(self.pos).write_buf(contents);
        }
    }

    pub fn erase_all(&mut self, attrs: crate::attrs::Attrs) {
        for row in self.drawing_rows_mut() {
            row.clear(attrs);
        }
    }

    pub fn erase_all_forward(&mut self, attrs: crate::attrs::Attrs) {
        let pos = self.pos;
        for row in self.drawing_rows_mut().skip(usize::from(pos.row) + 1) {
            row.clear(attrs);
        }

        self.erase_row_forward(attrs);
    }

    pub fn erase_all_backward(&mut self, attrs: crate::attrs::Attrs) {
        let pos = self.pos;
        for row in self.drawing_rows_mut().take(usize::from(pos.row)) {
            row.clear(attrs);
        }

        self.erase_row_backward(attrs);
    }

    pub fn erase_row(&mut self, attrs: crate::attrs::Attrs) {
        self.current_row_mut().clear(attrs);
    }

    pub fn erase_row_forward(&mut self, attrs: crate::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in pos.col..size.cols {
            row.erase(col, attrs);
        }
    }

    pub fn erase_row_backward(&mut self, attrs: crate::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in 0..=pos.col.min(size.cols - 1) {
            row.erase(col, attrs);
        }
    }

    pub fn insert_cells(&mut self, count: u16) {
        let size = self.size;
        let pos = self.pos;
        let wide = pos.col < size.cols
            && self
                .drawing_cell(pos)
                // we assume self.pos.row is always valid, and we know we are
                // not off the end of a row because we just checked pos.col <
                // size.cols
                .unwrap()
                .is_wide_continuation();
        let row = self.current_row_mut();
        for _ in 0..count {
            if wide {
                row.get_mut(pos.col).unwrap().set_wide_continuation(false);
            }
            row.insert(pos.col, crate::Cell::new());
            if wide {
                row.get_mut(pos.col).unwrap().set_wide_continuation(true);
            }
        }
        row.truncate(size.cols);
    }

    pub fn delete_cells(&mut self, count: u16) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for _ in 0..(count.min(size.cols - pos.col)) {
            row.remove(pos.col);
        }
        row.resize(size.cols, crate::Cell::new());
    }

    pub fn erase_cells(&mut self, count: u16, attrs: crate::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in pos.col..((pos.col.saturating_add(count)).min(size.cols)) {
            row.erase(col, attrs);
        }
    }

    pub fn insert_lines(&mut self, count: u16) {
        for _ in 0..count {
            self.rows.remove(usize::from(self.scroll_bottom));
            self.rows.insert(usize::from(self.pos.row), self.new_row());
            // self.scroll_bottom is maintained to always be a valid row
            self.rows[usize::from(self.scroll_bottom)].wrap(false);
        }
    }

    pub fn delete_lines(&mut self, count: u16) {
        for _ in 0..(count.min(self.size.rows - self.pos.row)) {
            self.rows
                .insert(usize::from(self.scroll_bottom) + 1, self.new_row());
            self.rows.remove(usize::from(self.pos.row));
        }
    }

    pub fn scroll_up(&mut self, count: u16) {
        for _ in 0..(count.min(self.size.rows - self.scroll_top)) {
            self.rows
                .insert(usize::from(self.scroll_bottom) + 1, self.new_row());
            let removed = self.rows.remove(usize::from(self.scroll_top));
            if self.scrollback_len > 0 && !self.scroll_region_active() {
                self.scrollback.push_back(removed);
                while self.scrollback.len() > self.scrollback_len {
                    self.scrollback.pop_front();
                }
                if self.scrollback_offset > 0 {
                    self.scrollback_offset =
                        self.scrollback.len().min(self.scrollback_offset + 1);
                }
            }
        }
    }

    pub fn scroll_down(&mut self, count: u16) {
        for _ in 0..count {
            self.rows.remove(usize::from(self.scroll_bottom));
            self.rows
                .insert(usize::from(self.scroll_top), self.new_row());
            // self.scroll_bottom is maintained to always be a valid row
            self.rows[usize::from(self.scroll_bottom)].wrap(false);
        }
    }

    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let bottom = bottom.min(self.size().rows - 1);
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        } else {
            self.scroll_top = 0;
            self.scroll_bottom = self.size().rows - 1;
        }
        self.pos.row = self.scroll_top;
        self.pos.col = 0;
    }

    fn in_scroll_region(&self) -> bool {
        self.pos.row >= self.scroll_top && self.pos.row <= self.scroll_bottom
    }

    fn scroll_region_active(&self) -> bool {
        self.scroll_top != 0 || self.scroll_bottom != self.size.rows - 1
    }

    pub fn set_origin_mode(&mut self, mode: bool) {
        self.origin_mode = mode;
        self.set_pos(Pos { row: 0, col: 0 });
    }

    pub fn row_inc_clamp(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_add(count);
        self.row_clamp_bottom(in_scroll_region);
    }

    pub fn row_inc_scroll(&mut self, count: u16) -> u16 {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_add(count);
        let lines = self.row_clamp_bottom(in_scroll_region);
        if in_scroll_region {
            self.scroll_up(lines);
            lines
        } else {
            0
        }
    }

    pub fn row_dec_clamp(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_sub(count);
        self.row_clamp_top(in_scroll_region);
    }

    pub fn row_dec_scroll(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        // need to account for clamping by both row_clamp_top and by
        // saturating_sub
        let extra_lines = count.saturating_sub(self.pos.row);
        self.pos.row = self.pos.row.saturating_sub(count);
        let lines = self.row_clamp_top(in_scroll_region);
        self.scroll_down(lines + extra_lines);
    }

    pub fn row_set(&mut self, i: u16) {
        self.pos.row = i;
        self.row_clamp();
    }

    pub fn col_inc(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_add(count);
    }

    pub fn col_inc_clamp(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_add(count);
        self.col_clamp();
    }

    pub fn col_dec(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_sub(count);
    }

    pub fn col_tab(&mut self) {
        self.pos.col -= self.pos.col % 8;
        self.pos.col += 8;
        self.col_clamp();
    }

    pub fn col_set(&mut self, i: u16) {
        self.pos.col = i;
        self.col_clamp();
    }

    pub fn col_wrap(&mut self, width: u16, wrap: bool) {
        if self.pos.col > self.size.cols - width {
            let mut prev_pos = self.pos;
            self.pos.col = 0;
            let scrolled = self.row_inc_scroll(1);
            prev_pos.row -= scrolled;
            let new_pos = self.pos;
            self.drawing_row_mut(prev_pos.row)
                // we assume self.pos.row is always valid, and so prev_pos.row
                // must be valid because it is always less than or equal to
                // self.pos.row
                .unwrap()
                .wrap(wrap && prev_pos.row + 1 == new_pos.row);
        }
    }

    fn row_clamp_top(&mut self, limit_to_scroll_region: bool) -> u16 {
        if limit_to_scroll_region && self.pos.row < self.scroll_top {
            let rows = self.scroll_top - self.pos.row;
            self.pos.row = self.scroll_top;
            rows
        } else {
            0
        }
    }

    fn row_clamp_bottom(&mut self, limit_to_scroll_region: bool) -> u16 {
        let bottom = if limit_to_scroll_region {
            self.scroll_bottom
        } else {
            self.size.rows - 1
        };
        if self.pos.row > bottom {
            let rows = self.pos.row - bottom;
            self.pos.row = bottom;
            rows
        } else {
            0
        }
    }

    fn row_clamp(&mut self) {
        if self.pos.row > self.size.rows - 1 {
            self.pos.row = self.size.rows - 1;
        }
    }

    pub(crate) fn col_clamp(&mut self) {
        if self.pos.col > self.size.cols - 1 {
            self.pos.col = self.size.cols - 1;
        }
    }
}

/// Returns the display-ordered row at global index `g` from the scrollback
/// (oldest first) followed by the visible rows. Used while rebuilding the grid
/// during reflow.
fn grid_row_at<'a>(
    sb_rows: &'a std::collections::VecDeque<crate::row::Row>,
    vis_rows: &'a [crate::row::Row],
    g: usize,
) -> &'a crate::row::Row {
    if g < sb_rows.len() {
        &sb_rows[g]
    } else {
        &vis_rows[g - sb_rows.len()]
    }
}

/// Returns whether a row contains no written cells (only unwritten padding).
fn grid_row_is_blank(row: &crate::row::Row) -> bool {
    (0..row.cols()).all(|c| row.get(c).is_some_and(|ce| !ce.has_contents()))
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Pos {
    pub row: u16,
    pub col: u16,
}
