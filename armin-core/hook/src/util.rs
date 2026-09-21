//! Small shared helpers: hashing, slugs, time, encoding, randomness.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current unix time in seconds (0 on clock failure).
pub fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// FNV-1a 64-bit — stable across processes (unlike std's SipHash), so event
/// IDs derived from it dedupe correctly when the same text is re-captured.
pub fn fnv64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// First 8 hex chars of the FNV-1a 64-bit hash.
pub fn short_hash(data: &str) -> String {
    format!("{:08x}", fnv64(data.as_bytes()) & 0xffff_ffff)
}

/// Slug for file names: non-alphanumerics collapse to single dashes.
pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Minimal percent-encoding for query-string values (unreserved set only).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Random 32-hex-char token from the OS (fallback: time+pid based, non-crypto).
pub fn random_token() -> String {
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let mut buf = [0u8; 16];
            if f.read_exact(&mut buf).is_ok() {
                return buf.iter().map(|b| format!("{b:02x}")).collect();
            }
        }
    }
    let seed = fnv64(format!("{}-{}", unix_secs(), std::process::id()).as_bytes());
    format!("{seed:016x}")
}

/// Truncate a string to at most `max` bytes on a char boundary.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\u{2026}", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv64_matches_known_vectors() {
        assert_eq!(fnv64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv64(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv64(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn slugify_collapses() {
        assert_eq!(slugify("https://github.com/a/b.git"), "https-github-com-a-b-git");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("a  b//c"), "a-b-c");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "héllo wörld";
        assert_eq!(truncate(s, 100), s);
        let t = truncate(s, 3);
        assert!(t.ends_with('\u{2026}'));
    }

    #[test]
    fn percent_encode_basic() {
        assert_eq!(percent_encode("a/b c"), "a%2Fb%20c");
        assert_eq!(percent_encode("a.b~c"), "a.b~c");
    }

    #[test]
    fn random_token_shape() {
        let t = random_token();
        assert!(!t.is_empty());
        assert_ne!(t, random_token());
    }
}
