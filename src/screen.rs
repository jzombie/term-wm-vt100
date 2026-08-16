use crate::term::BufWrite as _;
use unicode_width::UnicodeWidthChar as _;

const MODE_APPLICATION_KEYPAD: u8 = 0b0000_0001;
const MODE_APPLICATION_CURSOR: u8 = 0b0000_0010;
const MODE_HIDE_CURSOR: u8 = 0b0000_0100;
const MODE_ALTERNATE_SCREEN: u8 = 0b0000_1000;
const MODE_BRACKETED_PASTE: u8 = 0b0001_0000;
/// DECAWM — automatic wrap at the right margin (on by default).
const MODE_AUTOWRAP: u8 = 0b0010_0000;
/// IRM — insert mode (`CSI 4 h`/`l`): printable characters are inserted at the
/// cursor, shifting the rest of the row right, instead of overwriting. Editors
/// (pico/nano) use this to insert characters into the middle of a line.
const MODE_INSERT: u8 = 0b0100_0000;

/// The xterm mouse handling mode currently in use.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolMode {
    /// Mouse handling is disabled.
    #[default]
    None,

    /// Mouse button events should be reported on button press. Also known as
    /// X10 mouse mode.
    Press,

    /// Mouse button events should be reported on button press and release.
    /// Also known as VT200 mouse mode.
    PressRelease,

    // Highlight,
    /// Mouse button events should be reported on button press and release, as
    /// well as when the mouse moves between cells while a button is held
    /// down.
    ButtonMotion,

    /// Mouse button events should be reported on button press and release,
    /// and mouse motion events should be reported when the mouse moves
    /// between cells regardless of whether a button is held down or not.
    AnyMotion,
    // DecLocator,
}

/// The encoding to use for the enabled [`MouseProtocolMode`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolEncoding {
    /// Default single-printable-byte encoding.
    #[default]
    Default,

    /// UTF-8-based encoding.
    Utf8,

    /// SGR-like encoding.
    Sgr,
    // Urxvt,
}

/// Represents the overall terminal state.
#[derive(Clone, Debug)]
pub struct Screen {
    grid: crate::grid::Grid,
    alternate_grid: crate::grid::Grid,

    attrs: crate::attrs::Attrs,
    saved_attrs: crate::attrs::Attrs,

    modes: u8,
    mouse_protocol_mode: MouseProtocolMode,
    mouse_protocol_encoding: MouseProtocolEncoding,

    /// The currently active OSC 8 hyperlink (0 = none); stamped onto every
    /// cell written while it is active.
    hyperlink_id: u16,
    /// URI table indexed by `hyperlink_id` (index 0 is unused). A single
    /// entry is shared by every cell carrying the same hyperlink.
    hyperlink_table: Vec<std::sync::Arc<str>>,
    /// Dedupe map for [`hyperlink_table`](Self::hyperlink_table).
    hyperlink_map: std::collections::HashMap<std::sync::Arc<str>, u16>,
    /// The pane's current working directory as a decoded absolute filesystem
    /// path (from OSC 7), used to canonicalize relative OSC 8 targets.
    cwd: Option<String>,
}

impl Screen {
    pub(crate) fn new(
        size: crate::grid::Size,
        scrollback_len: usize,
    ) -> Self {
        let mut grid = crate::grid::Grid::new(size, scrollback_len);
        grid.allocate_rows();
        Self {
            grid,
            alternate_grid: crate::grid::Grid::new(size, 0),

            attrs: crate::attrs::Attrs::default(),
            saved_attrs: crate::attrs::Attrs::default(),

            modes: MODE_AUTOWRAP,
            mouse_protocol_mode: MouseProtocolMode::default(),
            mouse_protocol_encoding: MouseProtocolEncoding::default(),

            hyperlink_id: 0,
            hyperlink_table: Vec::new(),
            hyperlink_map: std::collections::HashMap::new(),
            cwd: None,
        }
    }

    /// Resizes the terminal.
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        // Reflow the main grid so soft-wrapped lines in the scrollback re-wrap
        // to the new width (information is preserved instead of truncated).
        // The alternate screen grid is NOT reflowed: alt-screen applications
        // (vim, tmux, etc.) lay out a 2D matrix via absolute cursor
        // positioning and repaint on SIGWINCH, so reflowing their grid only
        // corrupts the intermediate frame before the redraw.
        self.grid.set_size(crate::grid::Size { rows, cols }, true);
        self.alternate_grid
            .set_size(crate::grid::Size { rows, cols }, false);
    }

    /// Returns the current size of the terminal.
    ///
    /// The return value will be (rows, cols).
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        let size = self.grid().size();
        (size.rows, size.cols)
    }

    /// Scrolls to the given position in the scrollback.
    ///
    /// This position indicates the offset from the top of the screen, and
    /// should be `0` to put the normal screen in view.
    ///
    /// This affects the return values of methods called on the screen: for
    /// instance, `screen.cell(0, 0)` will return the top left corner of the
    /// screen after taking the scrollback offset into account.
    ///
    /// The value given will be clamped to the actual size of the scrollback.
    pub fn set_scrollback(&mut self, rows: usize) {
        self.grid_mut().set_scrollback(rows);
    }

    /// Returns the current position in the scrollback.
    ///
    /// This position indicates the offset from the top of the screen, and is
    /// `0` when the normal screen is in view.
    #[must_use]
    pub fn scrollback(&self) -> usize {
        self.grid().scrollback()
    }

    /// Returns the text contents of the terminal.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    #[must_use]
    pub fn contents(&self) -> String {
        let mut contents = String::new();
        self.write_contents(&mut contents);
        contents
    }

    fn write_contents(&self, contents: &mut String) {
        self.grid().write_contents(contents);
    }

    /// Returns the text contents of the terminal by row, restricted to the
    /// given subset of columns.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    ///
    /// Newlines will not be included.
    pub fn rows(
        &self,
        start: u16,
        width: u16,
    ) -> impl Iterator<Item = String> + '_ {
        self.grid().visible_rows().map(move |row| {
            let mut contents = String::new();
            row.write_contents(&mut contents, start, width, false);
            contents
        })
    }

    /// Returns the text contents of the terminal logically between two cells.
    /// This will include the remainder of the starting row after `start_col`,
    /// followed by the entire contents of the rows between `start_row` and
    /// `end_row`, followed by the beginning of the `end_row` up until
    /// `end_col`. This is useful for things like determining the contents of
    /// a clipboard selection.
    #[must_use]
    pub fn contents_between(
        &self,
        start_row: u16,
        start_col: u16,
        end_row: u16,
        end_col: u16,
    ) -> String {
        match start_row.cmp(&end_row) {
            std::cmp::Ordering::Less => {
                let (_, cols) = self.size();
                let mut contents = String::new();
                for (i, row) in self
                    .grid()
                    .visible_rows()
                    .enumerate()
                    .skip(usize::from(start_row))
                    .take(usize::from(end_row) - usize::from(start_row) + 1)
                {
                    if i == usize::from(start_row) {
                        row.write_contents(
                            &mut contents,
                            start_col,
                            cols - start_col,
                            false,
                        );
                        if !row.wrapped() {
                            contents.push('\n');
                        }
                    } else if i == usize::from(end_row) {
                        row.write_contents(&mut contents, 0, end_col, false);
                    } else {
                        row.write_contents(&mut contents, 0, cols, false);
                        if !row.wrapped() {
                            contents.push('\n');
                        }
                    }
                }
                contents
            }
            std::cmp::Ordering::Equal => {
                if start_col < end_col {
                    self.rows(start_col, end_col - start_col)
                        .nth(usize::from(start_row))
                        .unwrap_or_default()
                } else {
                    String::new()
                }
            }
            std::cmp::Ordering::Greater => String::new(),
        }
    }

    /// Return escape codes sufficient to reproduce the entire contents of the
    /// current terminal state. This is a convenience wrapper around
    /// [`contents_formatted`](Self::contents_formatted) and
    /// [`input_mode_formatted`](Self::input_mode_formatted).
    #[must_use]
    pub fn state_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_contents_formatted(&mut contents);
        self.write_input_mode_formatted(&mut contents);
        contents
    }

    /// Return escape codes sufficient to turn the terminal state of the
    /// screen `prev` into the current terminal state. This is a convenience
    /// wrapper around [`contents_diff`](Self::contents_diff) and
    /// [`input_mode_diff`](Self::input_mode_diff).
    #[must_use]
    pub fn state_diff(&self, prev: &Self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_contents_diff(&mut contents, prev);
        self.write_input_mode_diff(&mut contents, prev);
        contents
    }

    /// Returns the formatted visible contents of the terminal.
    ///
    /// Formatting information will be included inline as terminal escape
    /// codes. The result will be suitable for feeding directly to a raw
    /// terminal parser, and will result in the same visual output.
    #[must_use]
    pub fn contents_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_contents_formatted(&mut contents);
        contents
    }

    fn write_contents_formatted(&self, contents: &mut Vec<u8>) {
        crate::term::HideCursor::new(self.hide_cursor()).write_buf(contents);
        let prev_attrs = self.grid().write_contents_formatted(contents);
        self.attrs.write_escape_code_diff(contents, &prev_attrs);
    }

    /// Returns the formatted visible contents of the terminal by row,
    /// restricted to the given subset of columns.
    ///
    /// Formatting information will be included inline as terminal escape
    /// codes. The result will be suitable for feeding directly to a raw
    /// terminal parser, and will result in the same visual output.
    ///
    /// You are responsible for positioning the cursor before printing each
    /// row, and the final cursor position after displaying each row is
    /// unspecified.
    // the unwraps in this method shouldn't be reachable
    #[allow(clippy::missing_panics_doc)]
    pub fn rows_formatted(
        &self,
        start: u16,
        width: u16,
    ) -> impl Iterator<Item = Vec<u8>> + '_ {
        let mut wrapping = false;
        self.grid().visible_rows().enumerate().map(move |(i, row)| {
            // number of rows in a grid is stored in a u16 (see Size), so
            // visible_rows can never return enough rows to overflow here
            let i = i.try_into().unwrap();
            let mut contents = vec![];
            row.write_contents_formatted(
                &mut contents,
                start,
                width,
                i,
                wrapping,
                None,
                None,
            );
            if start == 0 && width == self.grid.size().cols {
                wrapping = row.wrapped();
            }
            contents
        })
    }

    /// Returns a terminal byte stream sufficient to turn the visible contents
    /// of the screen described by `prev` into the visible contents of the
    /// screen described by `self`.
    ///
    /// The result of rendering `prev.contents_formatted()` followed by
    /// `self.contents_diff(prev)` should be equivalent to the result of
    /// rendering `self.contents_formatted()`. This is primarily useful when
    /// you already have a terminal parser whose state is described by `prev`,
    /// since the diff will likely require less memory and cause less
    /// flickering than redrawing the entire screen contents.
    #[must_use]
    pub fn contents_diff(&self, prev: &Self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_contents_diff(&mut contents, prev);
        contents
    }

    fn write_contents_diff(&self, contents: &mut Vec<u8>, prev: &Self) {
        if self.hide_cursor() != prev.hide_cursor() {
            crate::term::HideCursor::new(self.hide_cursor())
                .write_buf(contents);
        }
        let prev_attrs = self.grid().write_contents_diff(
            contents,
            prev.grid(),
            prev.attrs,
        );
        self.attrs.write_escape_code_diff(contents, &prev_attrs);
    }

    /// Returns a sequence of terminal byte streams sufficient to turn the
    /// visible contents of the subset of each row from `prev` (as described
    /// by `start` and `width`) into the visible contents of the corresponding
    /// row subset in `self`.
    ///
    /// You are responsible for positioning the cursor before printing each
    /// row, and the final cursor position after displaying each row is
    /// unspecified.
    // the unwraps in this method shouldn't be reachable
    #[allow(clippy::missing_panics_doc)]
    pub fn rows_diff<'a>(
        &'a self,
        prev: &'a Self,
        start: u16,
        width: u16,
    ) -> impl Iterator<Item = Vec<u8>> + 'a {
        self.grid()
            .visible_rows()
            .zip(prev.grid().visible_rows())
            .enumerate()
            .map(move |(i, (row, prev_row))| {
                // number of rows in a grid is stored in a u16 (see Size), so
                // visible_rows can never return enough rows to overflow here
                let i = i.try_into().unwrap();
                let mut contents = vec![];
                row.write_contents_diff(
                    &mut contents,
                    prev_row,
                    start,
                    width,
                    i,
                    false,
                    false,
                    crate::grid::Pos { row: i, col: start },
                    crate::attrs::Attrs::default(),
                );
                contents
            })
    }

    /// Returns terminal escape sequences sufficient to set the current
    /// terminal's input modes.
    ///
    /// Supported modes are:
    /// * application keypad
    /// * application cursor
    /// * bracketed paste
    /// * xterm mouse support
    #[must_use]
    pub fn input_mode_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_input_mode_formatted(&mut contents);
        contents
    }

    fn write_input_mode_formatted(&self, contents: &mut Vec<u8>) {
        crate::term::ApplicationKeypad::new(
            self.mode(MODE_APPLICATION_KEYPAD),
        )
        .write_buf(contents);
        crate::term::ApplicationCursor::new(
            self.mode(MODE_APPLICATION_CURSOR),
        )
        .write_buf(contents);
        crate::term::BracketedPaste::new(self.mode(MODE_BRACKETED_PASTE))
            .write_buf(contents);
        crate::term::MouseProtocolMode::new(
            self.mouse_protocol_mode,
            MouseProtocolMode::None,
        )
        .write_buf(contents);
        crate::term::MouseProtocolEncoding::new(
            self.mouse_protocol_encoding,
            MouseProtocolEncoding::Default,
        )
        .write_buf(contents);
    }

    /// Returns terminal escape sequences sufficient to change the previous
    /// terminal's input modes to the input modes enabled in the current
    /// terminal.
    #[must_use]
    pub fn input_mode_diff(&self, prev: &Self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_input_mode_diff(&mut contents, prev);
        contents
    }

    fn write_input_mode_diff(&self, contents: &mut Vec<u8>, prev: &Self) {
        if self.mode(MODE_APPLICATION_KEYPAD)
            != prev.mode(MODE_APPLICATION_KEYPAD)
        {
            crate::term::ApplicationKeypad::new(
                self.mode(MODE_APPLICATION_KEYPAD),
            )
            .write_buf(contents);
        }
        if self.mode(MODE_APPLICATION_CURSOR)
            != prev.mode(MODE_APPLICATION_CURSOR)
        {
            crate::term::ApplicationCursor::new(
                self.mode(MODE_APPLICATION_CURSOR),
            )
            .write_buf(contents);
        }
        if self.mode(MODE_BRACKETED_PASTE) != prev.mode(MODE_BRACKETED_PASTE)
        {
            crate::term::BracketedPaste::new(self.mode(MODE_BRACKETED_PASTE))
                .write_buf(contents);
        }
        crate::term::MouseProtocolMode::new(
            self.mouse_protocol_mode,
            prev.mouse_protocol_mode,
        )
        .write_buf(contents);
        crate::term::MouseProtocolEncoding::new(
            self.mouse_protocol_encoding,
            prev.mouse_protocol_encoding,
        )
        .write_buf(contents);
    }

    /// Returns terminal escape sequences sufficient to set the current
    /// terminal's drawing attributes.
    ///
    /// Supported drawing attributes are:
    /// * fgcolor
    /// * bgcolor
    /// * bold
    /// * dim
    /// * italic
    /// * underline
    /// * inverse
    ///
    /// This is not typically necessary, since
    /// [`contents_formatted`](Self::contents_formatted) will leave
    /// the current active drawing attributes in the correct state, but this
    /// can be useful in the case of drawing additional things on top of a
    /// terminal output, since you will need to restore the terminal state
    /// without the terminal contents necessarily being the same.
    #[must_use]
    pub fn attributes_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_attributes_formatted(&mut contents);
        contents
    }

    fn write_attributes_formatted(&self, contents: &mut Vec<u8>) {
        crate::term::ClearAttrs.write_buf(contents);
        self.attrs.write_escape_code_diff(
            contents,
            &crate::attrs::Attrs::default(),
        );
    }

    /// Returns the current cursor position of the terminal.
    ///
    /// The return value will be (row, col).
    #[must_use]
    pub fn cursor_position(&self) -> (u16, u16) {
        let pos = self.grid().pos();
        (pos.row, pos.col)
    }

    /// Returns terminal escape sequences sufficient to set the current
    /// cursor state of the terminal.
    ///
    /// This is not typically necessary, since
    /// [`contents_formatted`](Self::contents_formatted) will leave
    /// the cursor in the correct state, but this can be useful in the case of
    /// drawing additional things on top of a terminal output, since you will
    /// need to restore the terminal state without the terminal contents
    /// necessarily being the same.
    ///
    /// Note that the bytes returned by this function may alter the active
    /// drawing attributes, because it may require redrawing existing cells in
    /// order to position the cursor correctly (for instance, in the case
    /// where the cursor is past the end of a row). Therefore, you should
    /// ensure to reset the active drawing attributes if necessary after
    /// processing this data, for instance by using
    /// [`attributes_formatted`](Self::attributes_formatted).
    #[must_use]
    pub fn cursor_state_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_cursor_state_formatted(&mut contents);
        contents
    }

    fn write_cursor_state_formatted(&self, contents: &mut Vec<u8>) {
        crate::term::HideCursor::new(self.hide_cursor()).write_buf(contents);
        self.grid()
            .write_cursor_position_formatted(contents, None, None);

        // we don't just call write_attributes_formatted here, because that
        // would still be confusing - consider the case where the user sets
        // their own unrelated drawing attributes (on a different parser
        // instance) and then calls cursor_state_formatted. just documenting
        // it and letting the user handle it on their own is more
        // straightforward.
    }

    /// Returns the [`Cell`](crate::Cell) object at the given location in the
    /// terminal, if it exists.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&crate::Cell> {
        self.grid().visible_cell(crate::grid::Pos { row, col })
    }

    /// Returns the canonical OSC 8 hyperlink URI for the cell at the given
    /// location, if any.
    ///
    /// Uses the same row/column indexing (including the scrollback offset) as
    /// [`cell`](Self::cell).
    #[must_use]
    pub fn hyperlink(&self, row: u16, col: u16) -> Option<std::sync::Arc<str>> {
        let id = self
            .grid()
            .visible_cell(crate::grid::Pos { row, col })?
            .hyperlink_id();
        if id == 0 {
            None
        } else {
            self.hyperlink_table
                .get(usize::from(id) - 1)
                .cloned()
        }
    }

    /// Returns the pane's current working directory as a percent-encoded
    /// `file://` URI (from OSC 7), if one has been reported.
    #[must_use]
    pub fn cwd(&self) -> Option<String> {
        self.cwd.as_ref().map(|p| {
            format!("file://{}", crate::uri::percent_encode_path(p))
        })
    }

    /// Seeds the pane's working directory from a raw filesystem path (e.g.
    /// the PTY spawn directory) so relative OSC 8 targets resolve before any
    /// program reports OSC 7.
    pub fn set_initial_cwd(&mut self, path: &str) {
        if path.starts_with('/') && self.cwd.is_none() {
            self.cwd = Some(path.to_string());
        }
    }

    /// Sets the active OSC 8 hyperlink from a raw URI (canonicalized against
    /// the pane's cwd). An empty or unresolvable URI closes the active link.
    pub(crate) fn set_active_hyperlink(&mut self, raw_uri: &str) {
        match crate::uri::canonicalize_uri(raw_uri, self.cwd.as_deref()) {
            Some(uri) if !uri.is_empty() => {
                let uri: std::sync::Arc<str> = std::sync::Arc::from(uri.as_str());
                let id = if let Some(id) = self.hyperlink_map.get(&uri) {
                    *id
                } else {
                    let id = u16::try_from(self.hyperlink_table.len() + 1)
                        .unwrap_or(u16::MAX);
                    if id != u16::MAX {
                        self.hyperlink_table.push(uri.clone());
                        self.hyperlink_map.insert(uri, id);
                    }
                    id
                };
                self.hyperlink_id = id;
            }
            _ => self.hyperlink_id = 0,
        }
    }

    /// Records the pane's working directory from an OSC 7 URI.
    pub(crate) fn set_cwd(&mut self, raw_uri: &str) {
        if let Some(path) = crate::uri::cwd_uri_to_path(raw_uri) {
            self.cwd = Some(path);
        }
    }

    /// Returns whether the text in row `row` should wrap to the next line.
    #[must_use]
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.grid()
            .visible_row(row)
            .is_some_and(crate::row::Row::wrapped)
    }

    /// Returns whether the alternate screen is currently in use.
    #[must_use]
    pub fn alternate_screen(&self) -> bool {
        self.mode(MODE_ALTERNATE_SCREEN)
    }

    /// Returns whether the terminal should be in application keypad mode.
    #[must_use]
    pub fn application_keypad(&self) -> bool {
        self.mode(MODE_APPLICATION_KEYPAD)
    }

    /// Returns whether the terminal should be in application cursor mode.
    #[must_use]
    pub fn application_cursor(&self) -> bool {
        self.mode(MODE_APPLICATION_CURSOR)
    }

    /// Returns whether the terminal should be in hide cursor mode.
    #[must_use]
    pub fn hide_cursor(&self) -> bool {
        self.mode(MODE_HIDE_CURSOR)
    }

    /// Returns whether the terminal should be in bracketed paste mode.
    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.mode(MODE_BRACKETED_PASTE)
    }

    /// Returns the currently active [`MouseProtocolMode`].
    #[must_use]
    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        self.mouse_protocol_mode
    }

    /// Returns the currently active [`MouseProtocolEncoding`].
    #[must_use]
    pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
        self.mouse_protocol_encoding
    }

    /// Returns the currently active foreground color.
    #[must_use]
    pub fn fgcolor(&self) -> crate::Color {
        self.attrs.fgcolor
    }

    /// Returns the currently active background color.
    #[must_use]
    pub fn bgcolor(&self) -> crate::Color {
        self.attrs.bgcolor
    }

    /// Returns whether newly drawn text should be rendered with the bold text
    /// attribute.
    #[must_use]
    pub fn bold(&self) -> bool {
        self.attrs.bold()
    }

    /// Returns whether newly drawn text should be rendered with the dim text
    /// attribute.
    #[must_use]
    pub fn dim(&self) -> bool {
        self.attrs.dim()
    }

    /// Returns whether newly drawn text should be rendered with the italic
    /// text attribute.
    #[must_use]
    pub fn italic(&self) -> bool {
        self.attrs.italic()
    }

    /// Returns whether newly drawn text should be rendered with the
    /// underlined text attribute.
    #[must_use]
    pub fn underline(&self) -> bool {
        self.attrs.underline()
    }

    /// Returns whether newly drawn text should be rendered with the inverse
    /// text attribute.
    #[must_use]
    pub fn inverse(&self) -> bool {
        self.attrs.inverse()
    }

    pub(crate) fn grid(&self) -> &crate::grid::Grid {
        if self.mode(MODE_ALTERNATE_SCREEN) {
            &self.alternate_grid
        } else {
            &self.grid
        }
    }

    fn grid_mut(&mut self) -> &mut crate::grid::Grid {
        if self.mode(MODE_ALTERNATE_SCREEN) {
            &mut self.alternate_grid
        } else {
            &mut self.grid
        }
    }

    fn enter_alternate_grid(&mut self) {
        self.grid_mut().set_scrollback(0);
        self.set_mode(MODE_ALTERNATE_SCREEN);
        self.alternate_grid.allocate_rows();
    }

    fn exit_alternate_grid(&mut self) {
        self.clear_mode(MODE_ALTERNATE_SCREEN);
    }

    fn save_cursor(&mut self) {
        self.grid_mut().save_cursor();
        self.saved_attrs = self.attrs;
    }

    fn restore_cursor(&mut self) {
        self.grid_mut().restore_cursor();
        self.attrs = self.saved_attrs;
    }

    fn set_mode(&mut self, mode: u8) {
        self.modes |= mode;
    }

    fn clear_mode(&mut self, mode: u8) {
        self.modes &= !mode;
    }

    fn mode(&self, mode: u8) -> bool {
        self.modes & mode != 0
    }

    fn set_mouse_mode(&mut self, mode: MouseProtocolMode) {
        self.mouse_protocol_mode = mode;
    }

    fn clear_mouse_mode(&mut self, mode: MouseProtocolMode) {
        if self.mouse_protocol_mode == mode {
            self.mouse_protocol_mode = MouseProtocolMode::default();
        }
    }

    fn set_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
        self.mouse_protocol_encoding = encoding;
    }

    fn clear_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
        if self.mouse_protocol_encoding == encoding {
            self.mouse_protocol_encoding = MouseProtocolEncoding::default();
        }
    }
}

/// Stamp a just-written cell with an OSC 8 hyperlink id (`0` = none).
fn link_cell(cell: &mut crate::Cell, link: u16) {
    cell.set_hyperlink(link);
}

impl Screen {
    pub(crate) fn text(&mut self, c: char) {
        let pos = self.grid().pos();
        let size = self.grid().size();
        let attrs = self.attrs;
        let link = self.hyperlink_id;

        let width = c.width();
        if width.is_none() && (u32::from(c)) < 256 {
            // don't even try to draw control characters
            return;
        }
        let width = width
            .unwrap_or(1)
            .try_into()
            // width() can only return 0, 1, or 2
            .unwrap();

        if !self.mode(MODE_AUTOWRAP) {
            // A wide character needs `width` contiguous cells. With autowrap
            // disabled there is no wrapping to fall back on, so clamp the start
            // column to the right margin to keep the continuation cell write
            // (and the cursor) strictly inside the row.
            if size.cols < width {
                return;
            }
            if pos.col > size.cols - width {
                self.grid_mut().col_set(size.cols - width);
            }
        }

        if self.mode(MODE_AUTOWRAP) {
            // it doesn't make any sense to wrap if the last column in a row
            // didn't already have contents. don't try to handle the case where a
            // character wraps because there was only one column left in the
            // previous row - literally everything handles this case differently,
            // and this is tmux behavior (and also the simplest). i'm open to
            // reconsidering this behavior, but only with a really good reason
            // (xterm handles this by introducing the concept of triple width
            // cells, which i really don't want to do).
            let mut wrap = false;
            if pos.col > size.cols - width {
                let last_cell = self
                    .grid()
                    .drawing_cell(crate::grid::Pos {
                        row: pos.row,
                        col: size.cols - 1,
                    })
                    // pos.row is valid, since it comes directly from
                    // self.grid().pos() which we assume to always have a valid
                    // row value. size.cols - 1 is also always a valid column.
                    .unwrap();
                if last_cell.has_contents()
                    || last_cell.is_wide_continuation()
                {
                    wrap = true;
                }
            }
            self.grid_mut().col_wrap(width, wrap);
        } else {
            // DECAWM disabled: never wrap. Clamp the cursor to the right margin
            // so a character at the last column is overwritten by the next one
            // rather than wrapping to the following row.
            self.grid_mut().col_clamp();
        }
        let pos = self.grid().pos();

        if width == 0 {
            if pos.col > 0 {
                let mut prev_cell = self
                    .grid_mut()
                    .drawing_cell_mut(crate::grid::Pos {
                        row: pos.row,
                        col: pos.col - 1,
                    })
                    // pos.row is valid, since it comes directly from
                    // self.grid().pos() which we assume to always have a
                    // valid row value. pos.col - 1 is valid because we just
                    // checked for pos.col > 0.
                    .unwrap();
                if prev_cell.is_wide_continuation() {
                    prev_cell = self
                        .grid_mut()
                        .drawing_cell_mut(crate::grid::Pos {
                            row: pos.row,
                            col: pos.col - 2,
                        })
                        // pos.row is valid, since it comes directly from
                        // self.grid().pos() which we assume to always have a
                        // valid row value. we know pos.col - 2 is valid
                        // because the cell at pos.col - 1 is a wide
                        // continuation character, which means there must be
                        // the first half of the wide character before it.
                        .unwrap();
                }
                prev_cell.append(c);
                link_cell(prev_cell, link);
            } else if pos.row > 0 {
                let prev_row = self
                    .grid()
                    .drawing_row(pos.row - 1)
                    // pos.row is valid, since it comes directly from
                    // self.grid().pos() which we assume to always have a
                    // valid row value. pos.row - 1 is valid because we just
                    // checked for pos.row > 0.
                    .unwrap();
                if prev_row.wrapped() {
                    let mut prev_cell = self
                        .grid_mut()
                        .drawing_cell_mut(crate::grid::Pos {
                            row: pos.row - 1,
                            col: size.cols - 1,
                        })
                        // pos.row is valid, since it comes directly from
                        // self.grid().pos() which we assume to always have a
                        // valid row value. pos.row - 1 is valid because we
                        // just checked for pos.row > 0. col of size.cols - 1
                        // is always valid.
                        .unwrap();
                    if prev_cell.is_wide_continuation() {
                        prev_cell = self
                            .grid_mut()
                            .drawing_cell_mut(crate::grid::Pos {
                                row: pos.row - 1,
                                col: size.cols - 2,
                            })
                            // pos.row is valid, since it comes directly from
                            // self.grid().pos() which we assume to always
                            // have a valid row value. pos.row - 1 is valid
                            // because we just checked for pos.row > 0. col of
                            // size.cols - 2 is valid because the cell at
                            // size.cols - 1 is a wide continuation character,
                            // so it must have the first half of the wide
                            // character before it.
                            .unwrap();
                    }
                    prev_cell.append(c);
                    link_cell(prev_cell, link);
                }
            }
        } else {
            if self
                .grid()
                .drawing_cell(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap()
                .is_wide_continuation()
            {
                let prev_cell = self
                    .grid_mut()
                    .drawing_cell_mut(crate::grid::Pos {
                        row: pos.row,
                        col: pos.col - 1,
                    })
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() immediately before this, which
                    // ensures that self.grid().pos().col has a valid value.
                    // pos.col - 1 is valid because the cell at pos.col is a
                    // wide continuation character, so it must have the first
                    // half of the wide character before it.
                    .unwrap();
                prev_cell.clear(attrs);
            }

            if self
                .grid()
                .drawing_cell(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap()
                .is_wide()
            {
                let next_cell = self
                    .grid_mut()
                    .drawing_cell_mut(crate::grid::Pos {
                        row: pos.row,
                        col: pos.col + 1,
                    })
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() immediately before this, which
                    // ensures that self.grid().pos().col has a valid value.
                    // pos.col + 1 is valid because the cell at pos.col is a
                    // wide character, so it must have the second half of the
                    // wide character after it.
                    .unwrap();
                next_cell.set(' ', attrs);
                link_cell(next_cell, link);
            }

            if self.mode(MODE_INSERT) && pos.col < size.cols {
                // IRM insert mode: shift the rest of the row right so the new
                // character is inserted at the cursor instead of overwriting
                // the existing cell (editors like pico use CSI 4h/4l for
                // mid-line insertion).
                self.grid_mut().insert_cells(1);
            }
            let cell = self
                .grid_mut()
                .drawing_cell_mut(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap();
            cell.set(c, attrs);
            link_cell(cell, link);
            if self.mode(MODE_AUTOWRAP) {
                self.grid_mut().col_inc(1);
            } else {
                // With autowrap disabled, writing the last column must not leave
                // the cursor past the margin (no pending-wrap state); the next
                // character overwrites the last column instead.
                self.grid_mut().col_inc_clamp(1);
            }
            if width > 1 {
                let pos = self.grid().pos();
                if self
                    .grid()
                    .drawing_cell(pos)
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() earlier, which ensures that
                    // self.grid().pos().col has a valid value. this is true
                    // even though we just called col_inc, because this branch
                    // only happens if width > 1, and col_wrap takes width
                    // into account.
                    .unwrap()
                    .is_wide()
                {
                    let next_next_pos = crate::grid::Pos {
                        row: pos.row,
                        col: pos.col + 1,
                    };
                    let next_next_cell = self
                        .grid_mut()
                        .drawing_cell_mut(next_next_pos)
                        // pos.row is valid because we assume
                        // self.grid().pos() to always have a valid row value.
                        // pos.col is valid because we called col_wrap()
                        // earlier, which ensures that self.grid().pos().col
                        // has a valid value. this is true even though we just
                        // called col_inc, because this branch only happens if
                        // width > 1, and col_wrap takes width into account.
                        // pos.col + 1 is valid because the cell at pos.col is
                        // wide, and so it must have the second half of the
                        // wide character after it.
                        .unwrap();
                    next_next_cell.clear(attrs);
                    if next_next_pos.col == size.cols - 1 {
                        self.grid_mut()
                            .drawing_row_mut(pos.row)
                            // we assume self.grid().pos().row is always valid
                            .unwrap()
                            .wrap(false);
                    }
                }
                let next_cell = self
                    .grid_mut()
                    .drawing_cell_mut(pos)
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() earlier, which ensures that
                    // self.grid().pos().col has a valid value. this is true
                    // even though we just called col_inc, because this branch
                    // only happens if width > 1, and col_wrap takes width
                    // into account.
                    .unwrap();
                next_cell.clear(crate::attrs::Attrs::default());
                next_cell.set_wide_continuation(true);
                link_cell(next_cell, link);
                if self.mode(MODE_AUTOWRAP) {
                    self.grid_mut().col_inc(1);
                } else {
                    self.grid_mut().col_inc_clamp(1);
                }
            }
        }
    }

    // control codes

    pub(crate) fn bs(&mut self) {
        self.grid_mut().col_dec(1);
    }

    pub(crate) fn tab(&mut self) {
        self.grid_mut().col_tab();
    }

    pub(crate) fn lf(&mut self) {
        self.grid_mut().row_inc_scroll(1);
    }

    pub(crate) fn vt(&mut self) {
        self.lf();
    }

    pub(crate) fn ff(&mut self) {
        self.lf();
    }

    pub(crate) fn cr(&mut self) {
        self.grid_mut().col_set(0);
    }

    // escape codes

    // ESC 7
    pub(crate) fn decsc(&mut self) {
        self.save_cursor();
    }

    // ESC 8
    pub(crate) fn decrc(&mut self) {
        self.restore_cursor();
    }

    // ESC =
    pub(crate) fn deckpam(&mut self) {
        self.set_mode(MODE_APPLICATION_KEYPAD);
    }

    // ESC >
    pub(crate) fn deckpnm(&mut self) {
        self.clear_mode(MODE_APPLICATION_KEYPAD);
    }

    // ESC M
    pub(crate) fn ri(&mut self) {
        self.grid_mut().row_dec_scroll(1);
    }

    // ESC c
    pub(crate) fn ris(&mut self) {
        *self = Self::new(self.grid.size(), self.grid.scrollback_len());
    }

    // csi codes

    // CSI @
    pub(crate) fn ich(&mut self, count: u16) {
        self.grid_mut().insert_cells(count);
    }

    // CSI A
    pub(crate) fn cuu(&mut self, offset: u16) {
        self.grid_mut().row_dec_clamp(offset);
    }

    // CSI B
    pub(crate) fn cud(&mut self, offset: u16) {
        self.grid_mut().row_inc_clamp(offset);
    }

    // CSI C
    pub(crate) fn cuf(&mut self, offset: u16) {
        self.grid_mut().col_inc_clamp(offset);
    }

    // CSI D
    pub(crate) fn cub(&mut self, offset: u16) {
        self.grid_mut().col_dec(offset);
    }

    // CSI E
    pub(crate) fn cnl(&mut self, offset: u16) {
        self.grid_mut().col_set(0);
        self.grid_mut().row_inc_clamp(offset);
    }

    // CSI F
    pub(crate) fn cpl(&mut self, offset: u16) {
        self.grid_mut().col_set(0);
        self.grid_mut().row_dec_clamp(offset);
    }

    // CSI G
    pub(crate) fn cha(&mut self, col: u16) {
        self.grid_mut().col_set(col - 1);
    }

    // CSI H
    pub(crate) fn cup(&mut self, (row, col): (u16, u16)) {
        self.grid_mut().set_pos(crate::grid::Pos {
            row: row - 1,
            col: col - 1,
        });
    }

    // CSI J
    pub(crate) fn ed(
        &mut self,
        mode: u16,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        let attrs = self.attrs;
        match mode {
            0 => self.grid_mut().erase_all_forward(attrs),
            1 => self.grid_mut().erase_all_backward(attrs),
            2 => self.grid_mut().erase_all(attrs),
            _ => unhandled(self),
        }
    }

    // CSI ? J
    pub(crate) fn decsed(
        &mut self,
        mode: u16,
        unhandled: impl FnMut(&mut Self),
    ) {
        self.ed(mode, unhandled);
    }

    // CSI K
    pub(crate) fn el(
        &mut self,
        mode: u16,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        let attrs = self.attrs;
        match mode {
            0 => {
                // If the cursor sits at the pending-wrap position (col == cols),
                // an erase-to-EOL would cover an empty range and leave the last
                // column stale. Clamp to the right margin so it is cleared.
                if self.grid().pos().col >= self.grid().size().cols {
                    self.grid_mut().col_clamp();
                }
                self.grid_mut().erase_row_forward(attrs);
            }
            1 => self.grid_mut().erase_row_backward(attrs),
            2 => self.grid_mut().erase_row(attrs),
            _ => unhandled(self),
        }
    }

    // CSI ? K
    pub(crate) fn decsel(
        &mut self,
        mode: u16,
        unhandled: impl FnMut(&mut Self),
    ) {
        self.el(mode, unhandled);
    }

    // CSI L
    pub(crate) fn il(&mut self, count: u16) {
        self.grid_mut().insert_lines(count);
    }

    // CSI M
    pub(crate) fn dl(&mut self, count: u16) {
        self.grid_mut().delete_lines(count);
    }

    // CSI P
    pub(crate) fn dch(&mut self, count: u16) {
        self.grid_mut().delete_cells(count);
    }

    // CSI S
    pub(crate) fn su(&mut self, count: u16) {
        self.grid_mut().scroll_up(count);
    }

    // CSI T
    pub(crate) fn sd(&mut self, count: u16) {
        self.grid_mut().scroll_down(count);
    }

    // CSI X
    pub(crate) fn ech(&mut self, count: u16) {
        let attrs = self.attrs;
        self.grid_mut().erase_cells(count, attrs);
    }

    // CSI d
    pub(crate) fn vpa(&mut self, row: u16) {
        self.grid_mut().row_set(row - 1);
    }

    // CSI h (non-private: Set Mode, e.g. IRM insert mode)
    pub(crate) fn sm(
        &mut self,
        params: &vte::Params,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        for param in params {
            match param {
                [4] => self.set_mode(MODE_INSERT),
                _ => unhandled(self),
            }
        }
    }

    // CSI l (non-private: Reset Mode, e.g. IRM insert mode)
    pub(crate) fn rm(
        &mut self,
        params: &vte::Params,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        for param in params {
            match param {
                [4] => self.clear_mode(MODE_INSERT),
                _ => unhandled(self),
            }
        }
    }

    // CSI ? h
    pub(crate) fn decset(
        &mut self,
        params: &vte::Params,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        for param in params {
            match param {
                [1] => self.set_mode(MODE_APPLICATION_CURSOR),
                [6] => self.grid_mut().set_origin_mode(true),
                [7] => self.set_mode(MODE_AUTOWRAP),
                [9] => self.set_mouse_mode(MouseProtocolMode::Press),
                [25] => self.clear_mode(MODE_HIDE_CURSOR),
                [47] => self.enter_alternate_grid(),
                [1000] => {
                    self.set_mouse_mode(MouseProtocolMode::PressRelease);
                }
                [1002] => {
                    self.set_mouse_mode(MouseProtocolMode::ButtonMotion);
                }
                [1003] => self.set_mouse_mode(MouseProtocolMode::AnyMotion),
                [1005] => {
                    self.set_mouse_encoding(MouseProtocolEncoding::Utf8);
                }
                [1006] => {
                    self.set_mouse_encoding(MouseProtocolEncoding::Sgr);
                }
                [1049] => {
                    self.decsc();
                    self.alternate_grid.clear();
                    self.enter_alternate_grid();
                }
                [2004] => self.set_mode(MODE_BRACKETED_PASTE),
                _ => unhandled(self),
            }
        }
    }

    // CSI ? l
    pub(crate) fn decrst(
        &mut self,
        params: &vte::Params,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        for param in params {
            match param {
                [1] => self.clear_mode(MODE_APPLICATION_CURSOR),
                [6] => self.grid_mut().set_origin_mode(false),
                [7] => {
                    self.clear_mode(MODE_AUTOWRAP);
                    // Cancel any pending wrap (cursor at col == size.cols): clamp
                    // back to the right margin so subsequent writes overwrite the
                    // last column instead of spilling onto the next row.
                    self.grid_mut().col_clamp();
                }
                [9] => self.clear_mouse_mode(MouseProtocolMode::Press),
                [25] => self.set_mode(MODE_HIDE_CURSOR),
                [47] => {
                    self.exit_alternate_grid();
                }
                [1000] => {
                    self.clear_mouse_mode(MouseProtocolMode::PressRelease);
                }
                [1002] => {
                    self.clear_mouse_mode(MouseProtocolMode::ButtonMotion);
                }
                [1003] => {
                    self.clear_mouse_mode(MouseProtocolMode::AnyMotion);
                }
                [1005] => {
                    self.clear_mouse_encoding(MouseProtocolEncoding::Utf8);
                }
                [1006] => {
                    self.clear_mouse_encoding(MouseProtocolEncoding::Sgr);
                }
                [1049] => {
                    self.exit_alternate_grid();
                    self.decrc();
                }
                [2004] => self.clear_mode(MODE_BRACKETED_PASTE),
                _ => unhandled(self),
            }
        }
    }

    // CSI m
    pub(crate) fn sgr(
        &mut self,
        params: &vte::Params,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        // XXX really i want to just be able to pass in a default Params
        // instance with a 0 in it, but vte doesn't allow creating new Params
        // instances
        if params.is_empty() {
            self.attrs = crate::attrs::Attrs::default();
            return;
        }

        let mut iter = params.iter();

        macro_rules! next_param {
            () => {
                match iter.next() {
                    Some(n) => n,
                    _ => return,
                }
            };
        }

        macro_rules! to_u8 {
            ($n:expr) => {
                if let Some(n) = u16_to_u8($n) {
                    n
                } else {
                    return;
                }
            };
        }

        macro_rules! next_param_u8 {
            () => {
                if let &[n] = next_param!() {
                    to_u8!(n)
                } else {
                    return;
                }
            };
        }

        loop {
            match next_param!() {
                [0] => self.attrs = crate::attrs::Attrs::default(),
                [1] => self.attrs.set_bold(),
                [2] => self.attrs.set_dim(),
                [3] => self.attrs.set_italic(true),
                [4] => self.attrs.set_underline(true),
                [7] => self.attrs.set_inverse(true),
                [22] => self.attrs.set_normal_intensity(),
                [23] => self.attrs.set_italic(false),
                [24] => self.attrs.set_underline(false),
                [27] => self.attrs.set_inverse(false),
                [n] if (30..=37).contains(n) => {
                    self.attrs.fgcolor = crate::Color::Idx(to_u8!(*n) - 30);
                }
                [38, 2, r, g, b] => {
                    self.attrs.fgcolor =
                        crate::Color::Rgb(to_u8!(*r), to_u8!(*g), to_u8!(*b));
                }
                [38, 5, i] => {
                    self.attrs.fgcolor = crate::Color::Idx(to_u8!(*i));
                }
                [38] => match next_param!() {
                    [2] => {
                        let r = next_param_u8!();
                        let g = next_param_u8!();
                        let b = next_param_u8!();
                        self.attrs.fgcolor = crate::Color::Rgb(r, g, b);
                    }
                    [5] => {
                        self.attrs.fgcolor =
                            crate::Color::Idx(next_param_u8!());
                    }
                    _ => {
                        unhandled(self);
                        return;
                    }
                },
                [39] => {
                    self.attrs.fgcolor = crate::Color::Default;
                }
                [n] if (40..=47).contains(n) => {
                    self.attrs.bgcolor = crate::Color::Idx(to_u8!(*n) - 40);
                }
                [48, 2, r, g, b] => {
                    self.attrs.bgcolor =
                        crate::Color::Rgb(to_u8!(*r), to_u8!(*g), to_u8!(*b));
                }
                [48, 5, i] => {
                    self.attrs.bgcolor = crate::Color::Idx(to_u8!(*i));
                }
                [48] => match next_param!() {
                    [2] => {
                        let r = next_param_u8!();
                        let g = next_param_u8!();
                        let b = next_param_u8!();
                        self.attrs.bgcolor = crate::Color::Rgb(r, g, b);
                    }
                    [5] => {
                        self.attrs.bgcolor =
                            crate::Color::Idx(next_param_u8!());
                    }
                    _ => {
                        unhandled(self);
                        return;
                    }
                },
                [49] => {
                    self.attrs.bgcolor = crate::Color::Default;
                }
                [n] if (90..=97).contains(n) => {
                    self.attrs.fgcolor = crate::Color::Idx(to_u8!(*n) - 82);
                }
                [n] if (100..=107).contains(n) => {
                    self.attrs.bgcolor = crate::Color::Idx(to_u8!(*n) - 92);
                }
                _ => unhandled(self),
            }
        }
    }

    // CSI r
    pub(crate) fn decstbm(&mut self, (top, bottom): (u16, u16)) {
        self.grid_mut().set_scroll_region(top - 1, bottom - 1);
    }
}

fn u16_to_u8(i: u16) -> Option<u8> {
    if i > u16::from(u8::MAX) {
        None
    } else {
        // safe because we just ensured that the value fits in a u8
        Some(i.try_into().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Parser;

    /// Every visible cell in `row` is empty.
    fn row_is_empty(screen: &Screen, row: u16, cols: u16) -> bool {
        (0..cols).all(|col| {
            screen
                .cell(row, col)
                .is_none_or(|c| c.contents().is_empty())
        })
    }

    /// DECAWM off: writing across the right margin must NOT wrap. The cursor
    /// clamps to the last column and subsequent characters overwrite it.
    #[test]
    fn decawn_off_clamps_to_right_margin() {
        let mut parser = Parser::new(24, 80, 0);
        parser.process(b"\x1b[?7l");

        let chars: Vec<u8> = (0..85)
            .map(|i| b'a' + u8::try_from(i % 26).unwrap())
            .collect();
        parser.process(&chars);

        let screen = parser.screen();
        assert_eq!(
            screen.cursor_position(),
            (0, 79),
            "cursor must stay at the right margin"
        );
        assert!(
            row_is_empty(screen, 1, 80),
            "row 1 must remain empty (no wrap)"
        );
        assert!(
            !screen.row_wrapped(0),
            "row 0 must not be marked as wrapped"
        );
        // 85th char (index 84): b'a' + 84 % 26 = 'g', overwritten 5 times at col 79.
        assert_eq!(screen.cell(0, 79).unwrap().contents(), "g");
    }

    /// Disabling DECAWM while a pending wrap is set must cancel it: the cursor
    /// clamps to the last column and the next character overwrites it.
    #[test]
    fn decawn_toggle_resets_pending_wrap() {
        // Part 1: with wrap on, the 81st char after 80 wraps to row 1
        // (proving the pending-wrap state is active).
        let mut wrapping = Parser::new(24, 80, 0);
        wrapping.process(&[b'a'; 80]);
        wrapping.process(b"X");
        assert_eq!(wrapping.screen().cell(0, 79).unwrap().contents(), "a");
        assert_eq!(
            wrapping.screen().cell(1, 0).unwrap().contents(),
            "X",
            "with wrap on the 81st char must wrap to row 1"
        );

        // Part 2: disable wrap while pending → cursor clamps to the margin and
        // the next char overwrites the last column instead of wrapping.
        let mut parser = Parser::new(24, 80, 0);
        parser.process(&[b'a'; 80]);
        parser.process(b"\x1b[?7l");
        assert_eq!(
            parser.screen().cursor_position(),
            (0, 79),
            "disabling wrap must clamp the pending-wrap cursor to the margin"
        );
        parser.process(b"X");
        assert_eq!(parser.screen().cursor_position(), (0, 79));
        assert_eq!(parser.screen().cell(0, 79).unwrap().contents(), "X");
        assert!(
            row_is_empty(parser.screen(), 1, 80),
            "the next character must overwrite the margin, not wrap"
        );
    }

    /// Replay a pico-style horizontal-scroll sequence and assert nothing spills
    /// to adjacent rows.
    #[test]
    fn long_line_horizontal_scroll_simulation() {
        let mut parser = Parser::new(24, 80, 0);
        // Editor manages its own columns: disable wrap.
        parser.process(b"\x1b[?7l");
        parser.process(b"\x1b[1;1H");

        // Write a line up to the margin.
        parser.process(&[b'a'; 79]);
        // Clear to end of line from the margin, then overwrite the margin cell.
        parser.process(b"\x1b[0K");
        parser.process(b"Z");
        // Restore wrap.
        parser.process(b"\x1b[?7h");

        let screen = parser.screen();
        assert!(
            row_is_empty(screen, 1, 80),
            "no characters may spill onto row 2"
        );
        assert_eq!(screen.cell(0, 79).unwrap().contents(), "Z");
    }

    /// EL 0 (`ESC[0K`) at the pending-wrap column must clear the last column
    /// instead of erasing an empty range.
    #[test]
    fn el_clears_last_column_in_pending_wrap_state() {
        let mut parser = Parser::new(24, 80, 0);
        parser.process(&[b'a'; 80]);
        assert_eq!(
            parser.screen().cursor_position(),
            (0, 80),
            "cursor must be at the pending-wrap column"
        );

        parser.process(b"\x1b[0K");
        assert!(
            parser.screen().cell(0, 79).unwrap().contents().is_empty(),
            "EL 0 must clear the last column even at the pending-wrap position"
        );
    }

    /// DECAWM off: writing a wide character at the right margin must not panic
    /// or spill onto the next row. The start column clamps back so the glyph's
    /// continuation cell stays inside the row.
    #[test]
    fn decawn_off_wide_char_at_right_margin_does_not_panic() {
        let mut parser = Parser::new(24, 80, 0);
        parser.process(b"\x1b[?7l");
        parser.process(b"\x1b[1;80H");

        parser.process("あ".as_bytes());

        let screen = parser.screen();
        assert_eq!(
            screen.cursor_position(),
            (0, 79),
            "cursor must stay at the right margin"
        );
        assert!(
            row_is_empty(screen, 1, 80),
            "row 1 must remain empty (no wrap)"
        );
        assert_eq!(
            screen.cell(0, 78).unwrap().contents(),
            "あ",
            "wide char must be drawn at cols-2"
        );
        assert!(
            screen.cell(0, 79).unwrap().is_wide_continuation(),
            "continuation cell must be set at the last column"
        );
    }

    /// DECAWM off: writing a wide character at cols-2 must not advance the
    /// cursor past the right margin (no pending-wrap state).
    #[test]
    fn decawn_off_wide_char_at_cols_minus_two_does_not_overshoot() {
        let mut parser = Parser::new(24, 80, 0);
        parser.process(b"\x1b[?7l");
        parser.process(b"\x1b[1;79H");

        parser.process("あ".as_bytes());

        let screen = parser.screen();
        assert_eq!(
            screen.cursor_position(),
            (0, 79),
            "cursor must be clamped to the margin, not 80"
        );
        assert!(row_is_empty(screen, 1, 80), "row 1 must remain empty");
        assert_eq!(screen.cell(0, 78).unwrap().contents(), "あ");
        assert!(
            screen.cell(0, 79).unwrap().is_wide_continuation(),
            "continuation cell must be set at the last column"
        );
    }

    /// IRM insert mode (`CSI 4 h`): a printable written at the cursor shifts the
    /// rest of the row right instead of overwriting. pico uses this to insert
    /// characters mid-line.
    #[test]
    fn insert_mode_shifts_row_right() {
        let mut parser = Parser::new(24, 80, 0);
        parser.process(b"hello");
        // Move to col 1 (0-based) and enable insert mode.
        parser.process(b"\x1b[1;2H\x1b[4h");
        parser.process(b"X");
        parser.process(b"\x1b[4l");

        let screen = parser.screen();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "h");
        assert_eq!(
            screen.cell(0, 1).unwrap().contents(),
            "X",
            "inserted char"
        );
        assert_eq!(
            screen.cell(0, 2).unwrap().contents(),
            "e",
            "original shifted right"
        );
        assert_eq!(screen.cell(0, 3).unwrap().contents(), "l");
        assert_eq!(screen.cell(0, 4).unwrap().contents(), "l");
        assert_eq!(
            screen.cell(0, 5).unwrap().contents(),
            "o",
            "row tail preserved"
        );
    }

    /// Without IRM (default), a printable overwrites the cell at the cursor.
    #[test]
    fn no_insert_mode_overwrites() {
        let mut parser = Parser::new(24, 80, 0);
        parser.process(b"hello");
        parser.process(b"\x1b[1;2H");
        parser.process(b"X");

        let screen = parser.screen();
        assert_eq!(
            screen.cell(0, 1).unwrap().contents(),
            "X",
            "overwrote col 1"
        );
        assert_eq!(screen.cell(0, 2).unwrap().contents(), "l", "not shifted");
    }

    // ── History pull-down on vertical grow ──

    /// Growing the terminal vertically reveals the most recent scrollback rows
    /// at the top (in chronological order) instead of padding blank rows at the
    /// bottom, keeping the cursor/prompt anchored to the bottom.
    #[test]
    fn grow_pulls_history_into_grid() {
        let mut parser = Parser::new(3, 10, 100);
        parser.process(b"A\r\nB\r\nC");
        parser.screen_mut().su(2); // push rows A,B into scrollback
        {
            let screen = parser.screen();
            assert_eq!(screen.cell(0, 0).unwrap().contents(), "C");
            assert_eq!(screen.scrollback(), 0, "at the tail");
        }

        parser.screen_mut().set_size(5, 10); // grow 2 rows, cols unchanged
        let screen = parser.screen();
        // Pulled history appears chronologically at the top.
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "A");
        assert_eq!(screen.cell(1, 0).unwrap().contents(), "B");
        assert_eq!(screen.cell(2, 0).unwrap().contents(), "C");
        assert_eq!(screen.scrollback(), 0, "still following the tail");
        assert_eq!(
            screen.cursor_position().0,
            4,
            "cursor anchored to the bottom"
        );
    }

    /// With no history to pull, growth stays top-anchored and pads blanks below.
    #[test]
    fn grow_without_scrollback_pads_blanks() {
        let mut parser = Parser::new(3, 10, 0); // no scrollback capacity
        parser.process(b"A\r\nB\r\nC");
        assert_eq!(parser.screen().scrollback(), 0);

        parser.screen_mut().set_size(5, 10);
        let screen = parser.screen();
        assert_eq!(
            screen.cell(0, 0).unwrap().contents(),
            "A",
            "top-anchored"
        );
        assert_eq!(screen.cell(1, 0).unwrap().contents(), "B");
        assert_eq!(screen.cell(2, 0).unwrap().contents(), "C");
        assert!(row_is_empty(screen, 3, 10), "blank rows padded below");
        assert!(row_is_empty(screen, 4, 10), "blank rows padded below");
    }

    /// The pull is bounded by how much history actually exists.
    #[test]
    fn grow_pull_is_bounded_by_history() {
        let mut parser = Parser::new(3, 10, 100);
        parser.process(b"A\r\nB\r\nC");
        parser.screen_mut().su(1); // 1 row (A) in history
        assert_eq!(parser.screen().scrollback(), 0);

        parser.screen_mut().set_size(8, 10); // grow by 5, only 1 row of history
        let screen = parser.screen();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "A", "pulled line");
        assert_eq!(screen.cell(1, 0).unwrap().contents(), "B");
        assert_eq!(screen.cell(2, 0).unwrap().contents(), "C");
        assert!(row_is_empty(screen, 3, 10), "remainder padded blank");
        assert!(row_is_empty(screen, 7, 10), "remainder padded blank");
    }

    /// A scrolled-up view is left top-anchored: no pull-down.
    #[test]
    fn scrolled_up_grow_does_not_pull() {
        let mut parser = Parser::new(3, 10, 100);
        parser.process(b"A\r\nB\r\nC");
        parser.screen_mut().su(2); // scrollback=[A,B], grid=[C,_,_]
        parser.screen_mut().set_scrollback(2); // viewport scrolled up
        let first_before =
            parser.screen().cell(0, 0).unwrap().contents().to_string();
        assert_eq!(
            parser.screen().scrollback(),
            2,
            "viewport is scrolled up"
        );

        parser.screen_mut().set_size(5, 10);
        let screen = parser.screen();
        assert_eq!(
            screen.scrollback(),
            2,
            "scroll offset preserved, no pull"
        );
        assert_eq!(
            screen.cell(0, 0).unwrap().contents(),
            first_before,
            "top-anchored: first visible row unchanged"
        );
    }

    /// Growing while the cursor is NOT at the bottom edge must not pull history
    /// (a top-anchored prompt stays put; the grid pads blanks below). This is
    /// what prevents the grow/shrink blank-multiplication loop.
    #[test]
    fn grow_cursor_not_at_bottom_does_not_pull() {
        let mut parser = Parser::new(3, 10, 100);
        parser.process(b"A\r\nB\r\nC");
        parser.screen_mut().su(2); // scrollback=[A,B], grid=[C,_,_]
        parser.process(b"\x1b[1;1H"); // move cursor to the top row
        assert_eq!(
            parser.screen().cursor_position().0,
            0,
            "cursor near the top"
        );

        parser.screen_mut().set_size(5, 10); // grow 2 rows
        let screen = parser.screen();
        assert_eq!(
            screen.cell(0, 0).unwrap().contents(),
            "C",
            "top-anchored: no history pulled above the prompt"
        );
        assert_eq!(
            screen.cell(1, 0).unwrap().contents(),
            "",
            "blank row below"
        );
        assert!(row_is_empty(screen, 1, 10), "blanks padded below");
        assert!(row_is_empty(screen, 4, 10), "blanks padded below");
        assert_eq!(screen.cursor_position().0, 0, "cursor left where it was");
    }
}
