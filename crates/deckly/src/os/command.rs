//! The Open action's text: a path to open or a command line to run.

/// Splits a command line into words. Quotes group words and backslashes
/// escape the next character, as in a shell; on Windows a backslash is a
/// path separator and stays. Nothing else is special: there are no pipes or
/// variables, and nothing runs through a shell. `None` when a quote is left
/// open.
pub fn split_words(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if q == c => quote = None,
            (None, '"' | '\'') => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            (Some('\''), c) => word.get_or_insert_default().push(c),
            (_, '\\') if !cfg!(windows) => word.get_or_insert_default().push(chars.next()?),
            (None, c) if c.is_whitespace() => words.extend(word.take()),
            (_, c) => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    quote.is_none().then_some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        split_words(line).unwrap()
    }

    #[test]
    fn splits_a_command_with_a_quoted_argument() {
        assert_eq!(
            words(r#"/venv/bin/python /x/control.py toggle "RGB Lights""#),
            ["/venv/bin/python", "/x/control.py", "toggle", "RGB Lights"]
        );
    }

    #[test]
    fn quotes_keep_empty_words_and_join_with_their_neighbours() {
        assert_eq!(words(r#"a "" b"#), ["a", "", "b"]);
        assert_eq!(words(r#"--name="two words"x"#), ["--name=two wordsx"]);
        assert_eq!(words("'it \"is\"' done"), ["it \"is\"", "done"]);
        assert!(words("   ").is_empty());
    }

    #[test]
    fn open_quotes_are_refused() {
        assert_eq!(split_words("say \"hello"), None);
        assert_eq!(split_words("say 'hello"), None);
    }

    #[test]
    fn backslashes_follow_the_platform() {
        if cfg!(windows) {
            assert_eq!(words(r"C:\tools\a.exe x"), [r"C:\tools\a.exe", "x"]);
        } else {
            assert_eq!(words(r"two\ words next"), ["two words", "next"]);
            assert_eq!(split_words("trailing\\"), None);
        }
    }
}
