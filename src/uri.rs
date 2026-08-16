//! OSC 8 / OSC 7 URI canonicalization and RFC 3986 percent-encoding.
//!
//! Host terminals (Zed, Ghostty, VS Code) require fully-qualified
//! `file://` URIs with absolute paths. Programs frequently emit relative or
//! schemeless targets (`src/main.rs:42:10`); this module resolves them
//! against the pane's current working directory and percent-encodes path
//! components so the emitted escape sequence is always well-formed.

/// Percent-encode a path for inclusion in a `file://` URI.
///
/// Keeps unreserved characters plus the path separators and URI sub-delims
/// (`/ : @ ! $ & ' ( ) * + , ; =`) unescaped so Windows drive letters and the
/// `:line:col` suffix survive; encodes spaces, `#`, `?`, `%`, control
/// characters, and every non-ASCII byte.
pub fn percent_encode_path(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if is_unreserved(b)
            || matches!(
                b,
                b'/' | b':' | b'@' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+'
                    | b',' | b';' | b'='
            )
        {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(hex_digit(b >> 4));
            out.push(hex_digit(b & 0x0f));
        }
    }
    out
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => char::from(b'0' + n),
        _ => char::from(b'A' + n - 10),
    }
}

/// Decode `%XX` escapes in a URI path back into bytes.
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            out.push((hex_value(bytes[i + 1]) << 4) | hex_value(bytes[i + 2]));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        _ => b - b'A' + 10,
    }
}

/// Common URI schemes whose scheme-colon is not a path line-suffix.
const KNOWN_SCHEMES: &[&str] = &[
    "file", "http", "https", "ftp", "ftps", "sftp", "mailto", "ssh", "git",
    "tel", "news", "irc", "ircs", "gopher", "ws", "wss", "x-man-page",
];

/// Returns the URI scheme (including the trailing `:`) if `s` begins with a
/// recognizable one.
///
/// A leading `letters...:` is only treated as a scheme when it names a known
/// scheme or is followed by `/` (hierarchical URIs). This keeps bare paths
/// with a `:line:col` suffix (e.g. `src/main.rs:42`) from being misclassified
/// as schemes.
fn scheme_of(s: &str) -> Option<&str> {
    let colon = s.find(':')?;
    let scheme = &s[..colon];
    if scheme.is_empty()
        || !scheme.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        return None;
    }
    let lower = scheme.to_ascii_lowercase();
    let known = KNOWN_SCHEMES.iter().any(|k| *k == lower);
    if known || s[colon + 1..].starts_with('/') {
        Some(&s[..=colon])
    } else {
        None
    }
}

/// Extract the decoded filesystem path from a `file://` URI.
fn file_uri_to_path(uri: &str) -> String {
    let rest = uri
        .strip_prefix("file://")
        .or_else(|| uri.strip_prefix("file:"))
        .unwrap_or(uri);
    // Drop any host component (everything up to the first '/').
    let path = match rest.find('/') {
        Some(idx) => &rest[idx..],
        None => return String::new(),
    };
    percent_decode(path)
}

/// Peel a trailing `:line` / `:line:col` suffix (Zed/VS Code convention) from
/// a bare path, returning `(path, suffix)`.
fn split_line_suffix(raw: &str) -> (String, String) {
    let mut suffix = String::new();
    let mut rest = raw;
    for _ in 0..2 {
        match rest.rfind(':') {
            Some(idx)
                if idx + 1 < rest.len()
                    && rest[idx + 1..].chars().all(|c| c.is_ascii_digit()) =>
            {
                let (head, tail) = rest.split_at(idx);
                suffix = format!(":{}{}", &tail[1..], suffix);
                rest = head;
            }
            _ => break,
        }
    }
    (rest.to_string(), suffix)
}

/// Canonicalize a raw OSC 8 URI for re-emission.
///
/// Returns `None` when the target is a relative path that cannot be resolved
/// against the pane's working directory (so a malformed link is never
/// emitted). An empty string is returned as-is; callers treat it as the OSC 8
/// closing sequence.
pub fn canonicalize_uri(raw: &str, cwd: Option<&str>) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(String::new());
    }
    if let Some(scheme) = scheme_of(raw) {
        if scheme != "file:" {
            // Non-file schemes (http(s):, mailto:, ...) pass through untouched.
            return Some(raw.to_string());
        }
        let path = file_uri_to_path(raw);
        return Some(format!("file://{}", percent_encode_path(&path)));
    }
    // Bare path, possibly with a :line[:col] suffix.
    let (path, suffix) = split_line_suffix(raw);
    if path.starts_with('/') {
        Some(format!(
            "file://{}{}",
            percent_encode_path(&path),
            suffix
        ))
    } else if let Some(cwd) = cwd {
        let joined = if cwd.ends_with('/') {
            format!("{cwd}{path}")
        } else {
            format!("{cwd}/{path}")
        };
        Some(format!(
            "file://{}{}",
            percent_encode_path(&joined),
            suffix
        ))
    } else {
        None
    }
}

/// Decode an OSC 7 working-directory URI into an absolute filesystem path.
pub fn cwd_uri_to_path(uri: &str) -> Option<String> {
    if !uri.starts_with("file:") {
        return None;
    }
    let path = file_uri_to_path(uri);
    if path.starts_with('/') {
        Some(path)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encodes_spaces_and_non_ascii() {
        assert_eq!(
            percent_encode_path("/tmp/My Folder/résumé.txt"),
            "/tmp/My%20Folder/r%C3%A9sum%C3%A9.txt"
        );
    }

    #[test]
    fn percent_encodes_hash_question_percent() {
        assert_eq!(percent_encode_path("a#b?c%d"), "a%23b%3Fc%25d");
    }

    #[test]
    fn keeps_slash_colon_and_subdelims() {
        assert_eq!(
            percent_encode_path("/a/b:c@d/e"),
            "/a/b:c@d/e"
        );
    }

    #[test]
    fn percent_decode_roundtrip() {
        assert_eq!(
            percent_decode("/tmp/My%20Folder/r%C3%A9sum%C3%A9.txt"),
            "/tmp/My Folder/résumé.txt"
        );
    }

    #[test]
    fn absolute_path_canonicalizes() {
        assert_eq!(
            canonicalize_uri("/Users/you/proj/src/main.rs:42:10", None),
            Some("file:///Users/you/proj/src/main.rs:42:10".to_string())
        );
    }

    #[test]
    fn relative_path_resolves_against_cwd() {
        assert_eq!(
            canonicalize_uri("src/main.rs:42:10", Some("/Users/you/proj")),
            Some("file:///Users/you/proj/src/main.rs:42:10".to_string())
        );
    }

    #[test]
    fn relative_path_without_cwd_is_dropped() {
        assert_eq!(canonicalize_uri("src/main.rs:42", None), None);
    }

    #[test]
    fn file_uri_localhost_normalized() {
        assert_eq!(
            canonicalize_uri("file://localhost/etc/hosts:1", None),
            Some("file:///etc/hosts:1".to_string())
        );
    }

    #[test]
    fn non_file_scheme_passes_through() {
        assert_eq!(
            canonicalize_uri("https://example.com/a?b", None),
            Some("https://example.com/a?b".to_string())
        );
    }

    #[test]
    fn empty_uri_is_close_sequence() {
        assert_eq!(canonicalize_uri("", None), Some(String::new()));
    }

    #[test]
    fn line_suffix_peeling() {
        assert_eq!(
            split_line_suffix("src/main.rs:42:10"),
            ("src/main.rs".to_string(), ":42:10".to_string())
        );
        assert_eq!(
            split_line_suffix("a.rs:42"),
            ("a.rs".to_string(), ":42".to_string())
        );
        assert_eq!(
            split_line_suffix("time:10am"),
            ("time:10am".to_string(), String::new())
        );
    }

    #[test]
    fn cwd_uri_decodes_path() {
        assert_eq!(
            cwd_uri_to_path("file:///Users/you/proj"),
            Some("/Users/you/proj".to_string())
        );
        assert_eq!(
            cwd_uri_to_path("file://localhost/tmp/a"),
            Some("/tmp/a".to_string())
        );
    }
}
