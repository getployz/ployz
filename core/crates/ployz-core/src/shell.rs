//! POSIX shell quoting for text that goes into a command another shell parses.

/// `value` as one POSIX shell word: single-quoted, each embedded `'` closed, double-quoted
/// and reopened. The empty string is `''`.
#[must_use]
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(all(test, unix))]
mod tests {
    use std::process::Command;

    #[test]
    fn a_word_is_quoted_in_single_quotes_and_a_single_quote_is_reopened() {
        assert_eq!(super::quote("plain"), "'plain'");
        assert_eq!(super::quote(""), "''");
        assert_eq!(super::quote("it's"), r#"'it'"'"'s'"#);
        assert_eq!(super::quote("a b $c"), "'a b $c'");
    }

    #[test]
    fn a_shell_reads_the_word_back_as_the_text() {
        for text in [
            "plain",
            "",
            "'",
            "it's",
            "a b",
            "$HOME `id` $(id)",
            "line\nbreak",
            "\\",
            "\"",
            "''",
        ] {
            let output = Command::new("sh")
                .args(["-c", &format!("printf %s {}", super::quote(text))])
                .output()
                .unwrap();
            assert!(output.status.success(), "{text:?}");
            assert_eq!(String::from_utf8(output.stdout).unwrap(), text);
        }
    }
}
