//! Split a start command into argv the way Railway does for image and Dockerfile
//! services: mattn/go-shellwords `Parse` with its default options.
//!
//! No variable expansion, no command substitution, no comments. Parsing stops at
//! the first unquoted `;` `&` `|` `<` `>` and drops the rest of the line, so
//! `a && b` runs `a`. These rough edges are Railway's and are copied on purpose.

use super::ConfigError;

/// Whether a pending word holds anything, mirroring go-shellwords' `argType`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Word {
    None,
    Plain,
    /// A closed quote: kept even when empty, so `''` is an empty argument.
    Quoted,
}

/// Splits `line` into argv.
///
/// # Errors
/// Returns ConfigError for an unclosed quote, backtick, `$(` or trailing
/// backslash, and for a `(` or `)` outside quotes that does not form `$(…)`.
pub fn split_start_command(line: &str) -> Result<Vec<String>, ConfigError> {
    let invalid = || {
        ConfigError::at(
            "startCommand",
            "Start command has an unclosed quote or a stray parenthesis",
        )
    };
    let mut args = Vec::new();
    let mut buf = String::new();
    let mut word = Word::None;
    let (mut escaped, mut double, mut single, mut backtick, mut dollar_paren) =
        (false, false, false, false, false);
    let mut token_quoted = false;
    let mut after_dollar = false;
    let mut flush = |buf: &mut String, word: &mut Word, token_quoted: &mut bool| {
        if *word == Word::Quoted || (*word != Word::None && !buf.is_empty()) {
            args.push(std::mem::take(buf));
        }
        buf.clear();
        *word = Word::None;
        *token_quoted = false;
    };
    for c in line.chars() {
        let prev_dollar = std::mem::take(&mut after_dollar);
        if escaped {
            escaped = false;
            token_quoted = true;
            word = Word::Plain;
            if backtick || dollar_paren {
                buf.push('\\');
                buf.push(c);
                continue;
            }
            buf.push(match c {
                't' => '\t',
                'n' => '\n',
                c => c,
            });
            continue;
        }
        if c == '\\' {
            if single {
                buf.push('\\');
            } else {
                escaped = true;
            }
            continue;
        }
        if matches!(c, ' ' | '\t' | '\r' | '\n') {
            if single || double || backtick || dollar_paren {
                buf.push(c);
            } else {
                flush(&mut buf, &mut word, &mut token_quoted);
            }
            continue;
        }
        match c {
            '`' if !single && !double && !dollar_paren => backtick = !backtick,
            ')' if !single && !double && !backtick => {
                if !dollar_paren {
                    return Err(invalid());
                }
                buf.push(')');
                dollar_paren = false;
                word = Word::Plain;
                continue;
            }
            '(' if !single && !double && !backtick => {
                if dollar_paren || !prev_dollar {
                    return Err(invalid());
                }
                dollar_paren = true;
                buf.push('(');
                continue;
            }
            '"' if !single && !dollar_paren && !backtick => {
                if double {
                    word = Word::Quoted;
                }
                double = !double;
                token_quoted = true;
                continue;
            }
            '\'' if !double && !dollar_paren && !backtick => {
                if single {
                    word = Word::Quoted;
                }
                single = !single;
                token_quoted = true;
                continue;
            }
            ';' | '&' | '|' | '<' | '>' if !(single || double || backtick || dollar_paren) => {
                // An unquoted run of digits before `>` is a file descriptor, not a word.
                if c == '>'
                    && !buf.is_empty()
                    && !token_quoted
                    && buf.bytes().all(|b| b.is_ascii_digit())
                {
                    word = Word::None;
                }
                break;
            }
            _ => {}
        }
        word = Word::Plain;
        buf.push(c);
        after_dollar = c == '$' && !(single || double || backtick || dollar_paren);
    }
    flush(&mut buf, &mut word, &mut token_quoted);
    if escaped || single || double || backtick || dollar_paren {
        return Err(invalid());
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::split_start_command as split;

    #[track_caller]
    fn argv(line: &str, expected: &[&str]) {
        assert_eq!(split(line).unwrap(), expected, "{line}");
    }

    #[test]
    fn matches_railway_observations() {
        argv("echo P1A && echo P1B", &["echo", "P1A"]);
        argv("echo P5 one > /tmp/x", &["echo", "P5", "one"]);
        argv(
            r#"echo P3 ${{RAILWAY_SERVICE_NAME}} ${RAILWAY_SERVICE_NAME} 'sq $X' "dq" \$Y | cat"#,
            &[
                "echo",
                "P3",
                "${{RAILWAY_SERVICE_NAME}}",
                "${RAILWAY_SERVICE_NAME}",
                "sq $X",
                "dq",
                "$Y",
            ],
        );
        argv(
            "cat /proc/1/cmdline; echo DF2 $RAILWAY_SERVICE_NAME",
            &["cat", "/proc/1/cmdline"],
        );
        argv(
            r#"/bin/sh -c "echo $PORT && echo hi""#,
            &["/bin/sh", "-c", "echo $PORT && echo hi"],
        );
    }

    #[test]
    fn follows_go_shellwords_edges() {
        argv("", &[]);
        argv("&& echo hi", &[]);
        argv("a '' \"\" b", &["a", "", "", "b"]);
        argv(r#"a "x\"y\$z" 'p\q'"#, &["a", "x\"y$z", r"p\q"]);
        argv(r"a\ b \t", &["a b", "\t"]);
        argv("a\n\tb\r c", &["a", "b", "c"]);
        argv("echo 2>&1", &["echo"]);
        argv("echo a2>x", &["echo", "a2"]);
        argv("echo '2'>x", &["echo", "2"]);
        argv("echo $(date +%s; x) y", &["echo", "$(date +%s; x)", "y"]);
        argv("echo `a b; c` d", &["echo", "`a b; c`", "d"]);
        argv(
            "echo # not a comment",
            &["echo", "#", "not", "a", "comment"],
        );
        argv("echo a; echo 'unclosed", &["echo", "a"]);
        for invalid in [
            "echo 'a", "echo \"a", "echo a\\", "echo (a)", "echo a)", "echo `a", "echo $(a",
        ] {
            assert!(split(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn redis_preset_keeps_its_shell_script_whole() {
        argv(
            r#"sh -c 'exec docker-entrypoint.sh redis-server --requirepass "$REDIS_PASSWORD" --save 60 1 --dir /data'"#,
            &[
                "sh",
                "-c",
                r#"exec docker-entrypoint.sh redis-server --requirepass "$REDIS_PASSWORD" --save 60 1 --dir /data"#,
            ],
        );
    }
}
