mod helpers;

/// OSC 8 with an absolute `file://` URI stamps every cell in the span.
#[test]
fn osc8_absolute_path_stamps_cells() {
    let mut parser = term_wm_vt100::Parser::new(24, 80, 0);
    parser.process(
        b"\x1b]8;;file:///etc/hosts:1:1\x1b\\hosts\x1b]8;;\x1b\\\n",
    );
    let screen = parser.screen();
    assert_eq!(
        screen.hyperlink(0, 0).as_deref(),
        Some("file:///etc/hosts:1:1")
    );
    assert_eq!(
        screen.hyperlink(0, 1).as_deref(),
        Some("file:///etc/hosts:1:1")
    );
    assert_eq!(
        screen.hyperlink(0, 2).as_deref(),
        Some("file:///etc/hosts:1:1")
    );
    assert_eq!(
        screen.hyperlink(0, 3).as_deref(),
        Some("file:///etc/hosts:1:1")
    );
    assert_eq!(
        screen.hyperlink(0, 4).as_deref(),
        Some("file:///etc/hosts:1:1")
    );
    // Text after the closing sequence is not linked.
    assert_eq!(screen.hyperlink(0, 5), None);
    assert_eq!(screen.hyperlink(1, 0), None);
}

/// Cells written after `8;;` (close) carry no hyperlink.
#[test]
fn osc8_close_clears_active_link() {
    let mut parser = term_wm_vt100::Parser::new(24, 80, 0);
    parser.process(b"\x1b]8;;file:///a\x1b\\AB\x1b]8;;\x1b\\C");
    let screen = parser.screen();
    assert_eq!(screen.hyperlink(0, 0).as_deref(), Some("file:///a"));
    assert_eq!(screen.hyperlink(0, 1).as_deref(), Some("file:///a"));
    assert_eq!(screen.hyperlink(0, 2), None);
}

/// A relative target resolves against the pane's cwd (from OSC 7).
#[test]
fn relative_path_resolves_against_osc7_cwd() {
    let mut parser = term_wm_vt100::Parser::new(24, 80, 0);
    parser.process(b"\x1b]7;file:///Users/you/proj\x1b\\");
    parser.process(b"\x1b]8;;src/main.rs:42:10\x1b\\main\x1b]8;;\x1b\\");
    let screen = parser.screen();
    assert_eq!(
        screen.hyperlink(0, 0).as_deref(),
        Some("file:///Users/you/proj/src/main.rs:42:10")
    );
}

/// A relative target with no known cwd is dropped (no malformed link).
#[test]
fn relative_path_without_cwd_is_dropped() {
    let mut parser = term_wm_vt100::Parser::new(24, 80, 0);
    parser.process(b"\x1b]8;;src/main.rs:42\x1b\\main\x1b]8;;\x1b\\");
    assert_eq!(parser.screen().hyperlink(0, 0), None);
}

/// `set_initial_cwd` seeds resolution before any OSC 7 is emitted.
#[test]
fn initial_cwd_seeds_resolution() {
    let mut parser = term_wm_vt100::Parser::new(24, 80, 0);
    parser.set_initial_cwd("/tmp/My Folder");
    parser.process(b"\x1b]8;;a.rs:1\x1b\\a\x1b]8;;\x1b\\");
    assert_eq!(
        parser.screen().hyperlink(0, 0).as_deref(),
        Some("file:///tmp/My%20Folder/a.rs:1")
    );
}

/// OSC 7 cwd is exposed (percent-encoded) via `Screen::cwd`.
#[test]
fn osc7_cwd_roundtrip() {
    let mut parser = term_wm_vt100::Parser::new(24, 80, 0);
    assert_eq!(parser.screen().cwd(), None);
    parser.process(b"\x1b]7;file:///tmp/My Folder\x1b\\");
    assert_eq!(
        parser.screen().cwd().as_deref(),
        Some("file:///tmp/My%20Folder")
    );
}

/// Same URI reused across cells shares a single table entry.
#[test]
fn identical_uris_dedupe() {
    let mut parser = term_wm_vt100::Parser::new(24, 80, 0);
    parser.process(
        b"\x1b]8;;file:///a\x1b\\ab\x1b]8;;\x1b\\\r\n\
          \x1b]8;;file:///a\x1b\\cd\x1b]8;;\x1b\\",
    );
    let screen = parser.screen();
    assert_eq!(screen.hyperlink(0, 0).as_deref(), Some("file:///a"));
    assert_eq!(screen.hyperlink(1, 0).as_deref(), Some("file:///a"));
}

/// `file://localhost/...` is normalized to `file:///...`.
#[test]
fn file_uri_localhost_normalized() {
    let mut parser = term_wm_vt100::Parser::new(24, 80, 0);
    parser.process(b"\x1b]8;;file://localhost/etc/hosts:1\x1b\\h\x1b]8;;\x1b\\");
    assert_eq!(
        parser.screen().hyperlink(0, 0).as_deref(),
        Some("file:///etc/hosts:1")
    );
}

/// Wide characters: the continuation cell inherits the leading cell's link.
#[test]
fn wide_char_continuation_inherits_link() {
    let mut parser = term_wm_vt100::Parser::new(24, 80, 0);
    // '界' is a wide (CJK) character.
    parser.process(b"\x1b]8;;file:///a\x1b\\\xe7\x95\x8c\x1b]8;;\x1b\\");
    let screen = parser.screen();
    assert_eq!(screen.hyperlink(0, 0).as_deref(), Some("file:///a"));
    assert_eq!(screen.hyperlink(0, 1).as_deref(), Some("file:///a"));
}

/// Links survive scrolling into the scrollback (cells carry their id).
#[test]
fn hyperlink_preserved_through_scrollback() {
    let mut parser = term_wm_vt100::Parser::new(2, 20, 10);
    parser.process(b"\x1b]8;;file:///scroll\x1b\\S\x1b]8;;\x1b\\\r\n\r\n");
    // The row holding `S` has scrolled out of the 2-row viewport.
    parser.screen_mut().set_scrollback(1);
    let screen = parser.screen();
    assert_eq!(screen.scrollback(), 1);
    assert_eq!(screen.hyperlink(0, 0).as_deref(), Some("file:///scroll"));
}
