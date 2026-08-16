fn main() {
    let mut p = term_wm_vt100::Parser::new(3, 80, 0);
    let bytes: &[u8] = b"\x1b]8;;file:///a\x1b\\ab\x1b]8;;\x1b\\\n\x1b]8;;file:///a\x1b\\cd\x1b]8;;\x1b\\";
    println!("len={}", bytes.len());
    p.process(bytes);
    let s = p.screen();
    let mut line = String::new();
    for c in 0..6u16 { line.push_str(s.cell(1, c).map(|x| x.contents()).unwrap_or("_")); }
    println!("row1: {line:?}");
    println!("r0c0 link={:?}", s.hyperlink(0,0).map(|x|x.to_string()));
}
