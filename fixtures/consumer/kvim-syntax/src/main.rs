use kvim_syntax::SyntaxHighlighter;

fn main() {
    let highlighter = SyntaxHighlighter::new();
    assert_eq!(highlighter.cached_languages(), 0);

    #[cfg(feature = "grammar-rust")]
    assert!(kvim_syntax::language("rust").is_some());
}
