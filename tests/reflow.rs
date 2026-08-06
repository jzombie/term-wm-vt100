// Tests for the reflow-on-resize behavior of the main grid. Reflow re-chunks
// soft-wrapped lines to the new width on resize so no content is truncated,
// while the alternate screen grid keeps the legacy truncate/pad behavior.

/// Counts every character written to the grid (cell contents only; unwritten
/// padding cells are skipped) across the whole scrollback + visible content.
/// Reflow must preserve this multiset exactly.
fn char_multiset(p: &mut vt100::Parser) -> std::collections::BTreeMap<char, usize> {
    let (rows, cols) = p.screen().size();
    let orig_sb = p.screen().scrollback();
    let mut map = std::collections::BTreeMap::new();
    let sb_len = {
        p.screen_mut().set_scrollback(usize::MAX);
        p.screen().scrollback()
    };
    let add_row = |p: &vt100::Parser, r: u16, map: &mut std::collections::BTreeMap<char, usize>| {
        for c in 0..cols {
            if let Some(cell) = p.screen().cell(r, c) {
                for ch in cell.contents().chars() {
                    *map.entry(ch).or_insert(0) += 1;
                }
            }
        }
    };
    // read each scrollback row once (at offset `sb_len - i` it is the first
    // row of the visible window)
    for i in 0..sb_len {
        p.screen_mut().set_scrollback(sb_len - i);
        add_row(p, 0, &mut map);
    }
    // then the visible rows
    p.screen_mut().set_scrollback(0);
    for r in 0..rows {
        add_row(p, r, &mut map);
    }
    p.screen_mut().set_scrollback(orig_sb);
    map
}

#[test]
fn shrink_rewraps_wrapped_visible_content_without_loss() {
    let mut p = vt100::Parser::new(3, 20, 0);
    p.process(b"0123456789012345678901234"); // 25 chars -> 2 rows at 20 cols
    assert_eq!(p.screen().rows(0, 20).next().unwrap(), "01234567890123456789");
    assert_eq!(p.screen().rows(0, 20).nth(1).unwrap(), "01234");

    p.screen_mut().set_size(3, 10);
    let rows: Vec<String> = p.screen().rows(0, 10).collect();
    assert_eq!(
        rows,
        vec![
            "0123456789".to_string(),
            "0123456789".to_string(),
            "01234".to_string(),
        ]
    );
    assert!(p.screen().row_wrapped(0));
    assert!(p.screen().row_wrapped(1));
    assert!(!p.screen().row_wrapped(2));
}

#[test]
fn grow_rejoins_previously_wrapped_lines() {
    let mut p = vt100::Parser::new(3, 20, 0);
    p.process(b"0123456789012345678901234"); // 25 chars at 20 cols
    p.screen_mut().set_size(3, 10);
    p.screen_mut().set_size(3, 20);
    let rows: Vec<String> = p.screen().rows(0, 20).collect();
    assert_eq!(rows[0], "01234567890123456789");
    assert_eq!(rows[1], "01234");
    assert!(p.screen().row_wrapped(0));
    assert!(!p.screen().row_wrapped(1));
}

#[test]
fn reflow_preserves_scrollback_content() {
    // scrollback budget large enough to hold the re-wrapped rows so no
    // content is evicted; the character multiset must survive shrink+grow.
    let mut p = vt100::Parser::new(3, 20, 30);
    for i in 0..6u32 {
        p.process(format!("line {i}: {}\r\n", "a".repeat(12)).as_bytes());
    }
    let before = char_multiset(&mut p);
    p.screen_mut().set_size(3, 8);
    let after_shrink = char_multiset(&mut p);
    assert_eq!(before, after_shrink, "shrink must not lose characters");
    p.screen_mut().set_size(3, 20);
    let after_grow = char_multiset(&mut p);
    assert_eq!(before, after_grow, "grow must not lose characters");
}

#[test]
fn wide_chars_are_not_split_across_rows() {
    let mut p = vt100::Parser::new(3, 6, 0);
    p.process("あああ".as_bytes()); // 3 wide chars = 6 columns
    assert_eq!(p.screen().contents(), "あああ");

    p.screen_mut().set_size(3, 4);
    let rows: Vec<String> = p.screen().rows(0, 4).collect();
    assert_eq!(rows[0], "ああ");
    assert_eq!(rows[1], "あ");
    // the wide character leading cells are preserved exactly
    assert_eq!(p.screen().cell(0, 0).unwrap().contents(), "あ");
    assert_eq!(p.screen().cell(1, 0).unwrap().contents(), "あ");
    assert!(!p.screen().cell(0, 2).unwrap().is_wide_continuation());
}

#[test]
fn wide_char_continuation_keeps_sgr_attrs() {
    let mut p = vt100::Parser::new(3, 6, 0);
    p.process("\x1b[31mあ\x1b[0m".as_bytes()); // red wide char at col 0
    p.screen_mut().set_size(3, 2);
    // the wide char re-wraps to row 0, cols 0-1; the continuation cell (col 1)
    // must inherit the red foreground of the lead cell.
    let lead = p.screen().cell(0, 0).unwrap();
    let cont = p.screen().cell(0, 1).unwrap();
    assert_eq!(lead.fgcolor(), vt100::Color::Idx(1));
    assert_eq!(cont.fgcolor(), vt100::Color::Idx(1));
    assert!(cont.is_wide_continuation());
}

#[test]
fn explicit_trailing_spaces_are_preserved() {
    let mut p = vt100::Parser::new(3, 10, 0);
    p.process(b"foo   "); // explicit trailing spaces
    p.screen_mut().set_size(3, 20);
    assert_eq!(p.screen().rows(0, 20).next().unwrap(), "foo   ");
}

#[test]
fn cursor_follows_content_through_reflow() {
    let mut p = vt100::Parser::new(5, 20, 0);
    p.process(b"0123456789");
    assert_eq!(p.screen().cursor_position(), (0, 10));
    p.screen_mut().set_size(5, 8);
    // 10 chars re-wrap to "01234567" / "89"; cursor was at stream index 10
    assert_eq!(p.screen().cursor_position(), (1, 2));
}

#[test]
fn cursor_in_short_early_wrapped_row() {
    let mut p = vt100::Parser::new(5, 20, 0);
    p.process(b"0123456789012345678901234"); // 25 chars, cursor at (1, 5)
    assert_eq!(p.screen().cursor_position(), (1, 5));
    p.screen_mut().set_size(5, 7);
    // 25 chars re-wrap to "0123456"/"7890123"/"4567890"/"1234"
    let rows: Vec<String> = p.screen().rows(0, 7).collect();
    assert_eq!(rows[3], "1234");
    assert_eq!(p.screen().cursor_position(), (3, 4));
}

#[test]
fn pending_wrap_cursor_survives_reflow() {
    let mut p = vt100::Parser::new(5, 20, 0);
    p.process(b"0123456789012345678901234567890123456789"); // 40 chars fills 2 rows, cursor pending at (1, 20)
    assert_eq!(p.screen().cursor_position(), (1, 20));
    p.screen_mut().set_size(5, 10);
    // re-wraps to 4 rows of 10; cursor at the end of the last row (pending)
    assert_eq!(p.screen().cursor_position(), (3, 10));
}

#[test]
fn saved_cursor_restores_same_cell_after_reflow() {
    let mut p = vt100::Parser::new(5, 20, 0);
    p.process(b"0123456789\x1b7abcde"); // DECSC at (0, 10), then cursor at (0, 15)
    p.screen_mut().set_size(5, 8);
    p.process(b"\x1b8"); // DECRC
    // saved pos at stream index 10 -> "01234567" / "89abcde" -> (1, 2)
    assert_eq!(p.screen().cursor_position(), (1, 2));
}

#[test]
fn alternate_screen_is_not_reflowed() {
    let mut p = vt100::Parser::new(3, 20, 0);
    p.process(b"\x1b[?1049h01234567890123456789abcdef"); // alt screen, wraps to 2 rows
    p.screen_mut().set_size(3, 10);
    // the alternate grid keeps legacy truncate behavior: rows are cut, not
    // re-wrapped
    let rows: Vec<String> = p.screen().rows(0, 10).collect();
    assert_eq!(rows[0], "0123456789");
    assert_eq!(rows[1], "abcdef");
}

#[test]
fn scrolled_up_viewport_stays_anchored() {
    let mut p = vt100::Parser::new(3, 20, 10);
    for i in 0..6u32 {
        p.process(format!("line {i}: {}\r\n", "a".repeat(12)).as_bytes());
    }
    p.screen_mut().set_scrollback(1);
    let offset_before = p.screen().scrollback();
    assert!(offset_before > 0);
    p.screen_mut().set_size(3, 8);
    // reflow must preserve a scrolled-back viewport (non-zero offset)
    assert!(p.screen().scrollback() > 0, "scrolled-up viewport must survive reflow");
}

#[test]
fn tab_stops_remain_valid_after_reflow() {
    let mut p = vt100::Parser::new(3, 20, 0);
    p.process(b"abcdefghij"); // cursor at (0, 10)
    p.screen_mut().set_size(3, 30); // grow width
    p.process(b"\t"); // tab past old width must not panic
    assert!(p.screen().cursor_position().1 > 10);
}

#[test]
fn cursor_preceded_by_wide_char_maps_correctly() {
    let mut p = vt100::Parser::new(5, 20, 0);
    p.process("あabcde".as_bytes()); // wide char (2 cols) + 5 chars; cursor at (0, 7)
    assert_eq!(p.screen().cursor_position(), (0, 7));
    p.screen_mut().set_size(5, 8);
    // content re-wraps; the wide char is at cols 0-1, so the cursor (7) is in
    // the same row after the wide char
    assert_eq!(p.screen().cursor_position(), (0, 7));
}
