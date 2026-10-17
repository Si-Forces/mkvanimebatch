//! Text predicates the core shares to read what a JavaScript view would.

/// Whether JavaScript's `trim`, `parseInt` and `\s` skip `c`: its WhiteSpace and LineTerminator,
/// which add U+FEFF to Rust's White_Space and lack U+0085.
pub(crate) fn is_js_whitespace(c: char) -> bool {
    c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace())
}

#[cfg(test)]
mod tests {
    #[test]
    fn js_whitespace_adds_bom_and_drops_nel() {
        assert!(super::is_js_whitespace('\u{feff}'));
        assert!(!super::is_js_whitespace('\u{85}'));
        assert!(super::is_js_whitespace(' '));
    }
}
