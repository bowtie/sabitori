//! Shared helper for encoding Rust strings as null-terminated UTF-16 wide
//! strings, as required by most Win32 APIs.
//!
//! Several modules (`autostart`, `theme`, `notify`, `single_instance`) all
//! need the same conversion, so it lives here once.

/// Encode `s` as a null-terminated UTF-16 wide string suitable for Win32
/// APIs that take `PCWSTR`.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_wide_basic_ascii() {
        let wide = to_wide("hello");
        assert_eq!(wide, vec![b'h' as u16, b'e' as u16, b'l' as u16, b'l' as u16, b'o' as u16, 0]);
    }

    #[test]
    fn to_wide_empty_string() {
        let wide = to_wide("");
        assert_eq!(wide, vec![0]);
    }

    #[test]
    fn to_wide_null_terminated() {
        let wide = to_wide("abc");
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(wide.len(), 4);
    }

    #[test]
    fn to_wide_non_ascii() {
        // "café" → c, a, f, é (U+00E9), null
        let wide = to_wide("café");
        assert_eq!(wide, vec![b'c' as u16, b'a' as u16, b'f' as u16, 0xE9, 0]);
    }

    #[test]
    fn to_wide_supplementary_plane() {
        // "𝕩" (U+1D569) → surrogate pair 0xD835, 0xDD69, null
        let wide = to_wide("𝕩");
        assert_eq!(wide, vec![0xD835, 0xDD69, 0]);
    }
}
