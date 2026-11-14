//! Deterministic regression tests for paths that dependency-only bumps
//! otherwise leave uncovered (keeps `cargo llvm-cov` from regressing).

use term_wm_vt100::Color::{self, Idx};
use term_wm_vt100::{MouseProtocolEncoding, MouseProtocolMode, Parser};

#[test]
fn default_callbacks_are_noops() {
    // Exercises the default `Callbacks` impls for resize requests and the
    // clipboard (`src/callbacks.rs`).
    let mut parser = Parser::default();
    parser.process(b"\x1b[8;24;80t");
    parser.process(b"\x1b]52;c;QUJD\x07");
    parser.process(b"\x1b]52;c;?\x07");
    assert_eq!(parser.screen().contents(), "");
}

#[test]
fn unhandled_private_csi_is_ignored() {
    // `CSI ?` with an unknown final byte must reach the `unhandled_csi`
    // fallback (`src/perform.rs`).
    let mut parser = Parser::default();
    parser.process(b"\x1b[?999X");
    assert_eq!(parser.screen().contents(), "");
}

#[test]
fn malformed_osc52_is_ignored() {
    // Neither an unknown clipboard selector nor non-base64 data may panic
    // or touch the screen (`src/perform.rs` OSC 52 fallback).
    let mut parser = Parser::default();
    parser.process(b"hello");
    parser.process(b"\x1b]52;!;QUJD\x07");
    parser.process(b"\x1b]52;c;!!!\x07");
    assert_eq!(parser.screen().contents(), "hello");
}

#[test]
fn parser_write_and_flush() {
    // Covers the `std::io::Write` impl (`src/parser.rs`).
    use std::io::Write as _;
    let mut parser = Parser::default();
    assert_eq!(parser.write(b"hi").unwrap(), 2);
    parser.flush().unwrap();
    assert_eq!(parser.screen().contents(), "hi");
}

#[test]
fn parser_callbacks_mut_is_accessible() {
    // Covers `Parser::callbacks_mut` (`src/parser.rs`).
    let mut parser = Parser::default();
    let _ = parser.callbacks_mut();
}

#[test]
fn screen_state_getters() {
    // Covers the trivial `Screen` state accessors (`src/screen.rs`).
    let parser = Parser::default();
    let screen = parser.screen();
    assert!(!screen.alternate_screen());
    assert!(!screen.application_keypad());
    assert!(!screen.application_cursor());
    assert!(!screen.hide_cursor());
    assert!(!screen.bracketed_paste());
    assert_eq!(screen.mouse_protocol_mode(), MouseProtocolMode::None);
    assert_eq!(
        screen.mouse_protocol_encoding(),
        MouseProtocolEncoding::Default
    );
    assert_eq!(screen.fgcolor(), Color::Default);
    assert_eq!(screen.bgcolor(), Color::Default);
    assert!(!screen.bold());
    assert!(!screen.dim());
    assert!(!screen.italic());
    assert!(!screen.underline());
    assert!(!screen.inverse());
    assert!(screen.visible_row(0).is_some());
    assert!(screen.cell(0, 0).is_some());
}

#[test]
fn erase_display_and_line_reject_out_of_range_modes() {
    // `ED 3` / `EL 3` fall through to the unhandled hook (`src/screen.rs`).
    let mut parser = Parser::default();
    parser.process(b"hello");
    parser.process(b"\x1b[3J");
    parser.process(b"\x1b[3K");
    assert_eq!(parser.screen().contents(), "hello");
}

#[test]
fn bare_sgr_resets_attributes() {
    // `CSI m` with no params resets the attrs (`src/screen.rs`).
    let mut parser = Parser::default();
    parser.process(b"\x1b[1m");
    assert!(parser.screen().bold());
    parser.process(b"\x1b[m");
    assert!(!parser.screen().bold());
}

#[test]
fn sgr_rejects_bad_extended_colors() {
    // Unknown `38`/`48` sub-modes hit the unhandled hook (`src/screen.rs`).
    let mut parser = Parser::default();
    parser.process(b"\x1b[31m");
    parser.process(b"\x1b[38;3m");
    parser.process(b"\x1b[48;3m");
    assert_eq!(parser.screen().fgcolor(), Idx(1));
}

#[test]
fn sgr_ignores_out_of_range_rgb() {
    // Values above `u8::MAX` bail out of SGR parsing (`src/screen.rs`).
    let mut parser = Parser::default();
    parser.process(b"\x1b[38;2;300;0;0m");
    assert_eq!(parser.screen().fgcolor(), Color::Default);
}

#[test]
fn wide_char_in_single_column_terminal_is_dropped() {
    // With autowrap off and fewer columns than the glyph width, the
    // character is dropped instead of panicking (`src/screen.rs`).
    let mut parser = Parser::new(24, 1, 0);
    parser.process(b"\x1b[?7l");
    parser.process("Ｈ".as_bytes());
    assert_eq!(parser.screen().cursor_position(), (0, 0));
}

#[test]
fn contents_between_spanning_rows_keeps_newlines() {
    // Multi-row selections join unwrapped rows with `\n` (`src/screen.rs`).
    let mut parser = Parser::new(24, 80, 0);
    parser.process(b"aaa\r\nbbb\r\nccc");
    assert_eq!(
        parser.screen().contents_between(0, 0, 2, 3),
        "aaa\nbbb\nccc"
    );
}

#[test]
fn input_mode_diff_covers_mouse_teardown() {
    // Tearing down mouse modes/reporting emits the `?..l` sequences
    // (`src/term.rs`).
    let off = Parser::default();

    let mut press = Parser::default();
    press.process(b"\x1b[?9h");
    assert_eq!(
        press.screen().mouse_protocol_mode(),
        MouseProtocolMode::Press
    );
    assert_eq!(off.screen().input_mode_diff(press.screen()), b"\x1b[?9l");

    let mut motion = Parser::default();
    motion.process(b"\x1b[?1002h");
    assert_eq!(
        motion.screen().mouse_protocol_mode(),
        MouseProtocolMode::ButtonMotion
    );
    assert_eq!(
        off.screen().input_mode_diff(motion.screen()),
        b"\x1b[?1002l"
    );

    let mut utf8 = Parser::default();
    utf8.process(b"\x1b[?1005h");
    assert_eq!(
        utf8.screen().mouse_protocol_encoding(),
        MouseProtocolEncoding::Utf8
    );
    assert_eq!(off.screen().input_mode_diff(utf8.screen()), b"\x1b[?1005l");
}
