//! The three wire formats, named for what consumes them.
//!
//! Each is a distinct encoding and using one where another belongs corrupts a backup
//! at exit 0. `git hash-object --stdin-paths` has no `-z`, so a newline in a filename
//! splits one path into two; `git update-index -z --index-info` takes raw bytes, and
//! without `-z` a path starting with `"` is silently dequoted.

use crate::primitives::oid::Oid;
use std::path::Path;

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// A tree entry's mode. Layer 1 classifies `st_mode` and maps into this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileMode {
    Regular,
    Executable,
    Symlink,
}

impl FileMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
            Self::Symlink => "120000",
        }
    }
}

/// Percent-encodes one component of a refname.
///
/// Every byte outside `[A-Za-z0-9._-]` becomes `%XX`, `%` included. A leading `.`
/// and the dot of a trailing `.lock` are encoded too: both are legal bytes in
/// positions git rejects.
#[must_use]
pub fn percent_component(raw: &[u8]) -> String {
    let lock_dot = raw.ends_with(b".lock").then(|| raw.len() - 5);
    let mut out = String::with_capacity(raw.len());
    for (index, &byte) in raw.iter().enumerate() {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
        let forced = (index == 0 && byte == b'.') || Some(index) == lock_dot;
        if unreserved && !forced {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    out
}

/// Reverses [`percent_component`]. `None` when a `%` is not followed by two hex
/// digits.
#[must_use]
pub fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let raw = text.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' {
            let high = unhex(*raw.get(index + 1)?)?;
            let low = unhex(*raw.get(index + 2)?)?;
            out.push((high << 4) | low);
            index += 3;
        } else {
            out.push(raw[index]);
            index += 1;
        }
    }
    Some(out)
}

/// One line for `git hash-object --stdin-paths`, newline included.
///
/// Emitted raw when every byte is printable ASCII other than `"` and `\`, and in
/// git's C-quoted form otherwise. Git unquotes a line only when it begins with `"`,
/// and reads lines with `strbuf_getline`, which strips a trailing `\r` - so a path
/// beginning with a quote or ending in a carriage return must be quoted even though
/// neither byte is itself unprintable.
#[must_use]
pub fn stdin_paths_line(path: &Path) -> Vec<u8> {
    let raw = path.as_os_str().as_encoded_bytes();
    if raw
        .iter()
        .all(|&byte| (0x20..=0x7e).contains(&byte) && byte != b'"' && byte != b'\\')
    {
        let mut line = raw.to_vec();
        line.push(b'\n');
        return line;
    }
    let mut line = Vec::with_capacity(raw.len() + 3);
    line.push(b'"');
    for &byte in raw {
        match byte {
            0x07 => line.extend_from_slice(br"\a"),
            0x08 => line.extend_from_slice(br"\b"),
            0x09 => line.extend_from_slice(br"\t"),
            0x0a => line.extend_from_slice(br"\n"),
            0x0b => line.extend_from_slice(br"\v"),
            0x0c => line.extend_from_slice(br"\f"),
            0x0d => line.extend_from_slice(br"\r"),
            b'"' => line.extend_from_slice(b"\\\""),
            b'\\' => line.extend_from_slice(br"\\"),
            0x20..=0x7e => line.push(byte),
            _ => {
                line.push(b'\\');
                line.push(b'0' + (byte >> 6));
                line.push(b'0' + ((byte >> 3) & 0x07));
                line.push(b'0' + (byte & 0x07));
            }
        }
    }
    line.push(b'"');
    line.push(b'\n');
    line
}

/// One record for `git update-index -z --index-info`, NUL included.
///
/// The path is raw bytes with no quoting, which is what `-z` is for.
#[must_use]
pub fn index_info_line(mode: FileMode, oid: Oid, path: &Path) -> Vec<u8> {
    let raw = path.as_os_str().as_encoded_bytes();
    let mut line = Vec::with_capacity(raw.len() + 50);
    line.extend_from_slice(mode.as_str().as_bytes());
    line.push(b' ');
    line.extend_from_slice(oid.to_string().as_bytes());
    line.push(b'\t');
    line.extend_from_slice(raw);
    line.push(0);
    line
}

fn unhex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{FileMode, index_info_line, percent_component, percent_decode, stdin_paths_line};
    use crate::primitives::oid::Oid;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    fn path(raw: &[u8]) -> &Path {
        Path::new(OsStr::from_bytes(raw))
    }

    fn line(text: &[u8]) -> Vec<u8> {
        let mut out = text.to_vec();
        out.push(b'\n');
        out
    }

    #[test]
    fn percent_leaves_the_unreserved_set_alone() {
        assert_eq!(percent_component(b"handbook"), "handbook");
        assert_eq!(percent_component(b"v1.0_beta"), "v1.0_beta");
    }

    #[test]
    fn percent_encodes_the_documented_hostile_cases() {
        assert_eq!(percent_component(b"my docs"), "my%20docs");
        assert_eq!(percent_component(b"a/b"), "a%2Fb");
        assert_eq!(percent_component(b"100%"), "100%25");
        assert_eq!(percent_component(b"%"), "%25");
        assert_eq!(percent_component(b".config"), "%2Econfig");
        assert_eq!(percent_component(b"main.lock"), "main%2Elock");
        assert_eq!(percent_component(b".lock"), "%2Elock");
        assert_eq!(percent_component(b"caf\xc3\xa9"), "caf%C3%A9");
        assert_eq!(percent_component(b"a\nb"), "a%0Ab");
    }

    #[test]
    fn percent_round_trips_every_single_byte() {
        for byte in 0..=u8::MAX {
            let raw = [byte];
            let encoded = percent_component(&raw);
            assert_eq!(
                percent_decode(&encoded).as_deref(),
                Some(&raw[..]),
                "byte {byte:#04x} encoded as {encoded}"
            );
        }
    }

    #[test]
    fn percent_round_trips_every_byte_pair() {
        for high in 0..=u8::MAX {
            for low in 0..=u8::MAX {
                let raw = [high, low];
                let encoded = percent_component(&raw);
                assert_eq!(percent_decode(&encoded).as_deref(), Some(&raw[..]));
            }
        }
    }

    #[test]
    fn percent_output_is_always_a_safe_refname_component() {
        for high in 0..=u8::MAX {
            for low in 0..=u8::MAX {
                let encoded = percent_component(&[high, low]);
                assert!(
                    encoded.bytes().all(
                        |b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'%')
                    ),
                    "{encoded}"
                );
                assert!(!encoded.starts_with('.'), "{encoded}");
                assert!(!encoded.ends_with(".lock"), "{encoded}");
            }
        }
    }

    #[test]
    fn percent_decode_rejects_a_truncated_escape() {
        assert_eq!(percent_decode("%"), None);
        assert_eq!(percent_decode("%2"), None);
        assert_eq!(percent_decode("%zz"), None);
    }

    #[test]
    fn a_plain_path_is_emitted_raw() {
        assert_eq!(stdin_paths_line(path(b"a/b.md")), b"a/b.md\n");
    }

    #[test]
    fn a_hostile_path_is_c_quoted() {
        assert_eq!(stdin_paths_line(path(b"a\nb")), line(br#""a\nb""#));
        assert_eq!(stdin_paths_line(path(b"a\tb")), line(br#""a\tb""#));
        assert_eq!(stdin_paths_line(path(b"a\\b")), line(br#""a\\b""#));
        assert_eq!(stdin_paths_line(path(b"a\"b")), line(br#""a\"b""#));
        assert_eq!(
            stdin_paths_line(path(b"caf\xc3\xa9")),
            line(br#""caf\303\251""#)
        );
        assert_eq!(stdin_paths_line(path(b"a\xffb")), line(br#""a\377b""#));
    }

    #[test]
    fn a_quote_or_carriage_return_forces_quoting_even_though_both_are_printable() {
        assert_eq!(stdin_paths_line(path(b"\"a")), line(br#""\"a""#));
        assert_eq!(stdin_paths_line(path(b"a\r")), line(br#""a\r""#));
    }

    #[test]
    fn index_info_uses_raw_bytes_and_a_nul() {
        let oid = Oid::parse("8f2a10c1930b99aef686f41c8ee24e10f92aa7d4").expect("valid");
        let line = index_info_line(FileMode::Regular, oid, path(b"a\nb"));
        assert_eq!(
            line,
            b"100644 8f2a10c1930b99aef686f41c8ee24e10f92aa7d4\ta\nb\0"
        );
    }

    #[test]
    fn modes_are_the_three_git_stores() {
        assert_eq!(FileMode::Regular.as_str(), "100644");
        assert_eq!(FileMode::Executable.as_str(), "100755");
        assert_eq!(FileMode::Symlink.as_str(), "120000");
    }
}
