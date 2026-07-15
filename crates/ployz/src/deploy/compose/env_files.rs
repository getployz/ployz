use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvFileSource {
    pub path: String,
    pub required: EnvFileRequired,
    pub contents: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvFileRequired {
    Required,
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvFileMergeIssue {
    MissingRequired { path: String },
    Parse { path: String, message: String },
}

pub(crate) fn merge_env_files(
    sources: &[EnvFileSource],
) -> (BTreeMap<String, String>, Vec<EnvFileMergeIssue>) {
    let mut env = BTreeMap::new();
    let mut issues = Vec::new();
    for source in sources {
        let Some(contents) = &source.contents else {
            match source.required {
                EnvFileRequired::Required => issues.push(EnvFileMergeIssue::MissingRequired {
                    path: source.path.clone(),
                }),
                EnvFileRequired::Optional => {}
            }
            continue;
        };
        match parse_dotenv(contents) {
            Ok(parsed) => env.extend(parsed),
            Err(error) => issues.push(EnvFileMergeIssue::Parse {
                path: source.path.clone(),
                message: error.to_string(),
            }),
        }
    }
    (env, issues)
}

pub(crate) fn parse_dotenv(source: &str) -> Result<BTreeMap<String, String>, DotenvError> {
    let mut values = BTreeMap::new();
    for (line_index, raw_line) in source.lines().enumerate() {
        let line_number = line_index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, raw_value)) = line.split_once('=') else {
            return Err(DotenvError {
                line: line_number,
                message: "expected KEY=VALUE".to_owned(),
            });
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(DotenvError {
                line: line_number,
                message: "key is empty".to_owned(),
            });
        }
        values.insert(key.to_owned(), parse_value(raw_value.trim(), line_number)?);
    }
    Ok(values)
}

fn parse_value(value: &str, line: usize) -> Result<String, DotenvError> {
    let quote = match value.as_bytes() {
        [b'"', ..] => '"',
        [b'\'', ..] => '\'',
        _ => return Ok(strip_inline_comment(value).trim_end().to_owned()),
    };
    let body = &value[1..];
    let Some(close) = find_closing_quote(body, quote) else {
        return Err(DotenvError {
            line,
            message: "unterminated quoted value".to_owned(),
        });
    };
    let inner = &body[..close];
    let after = body[close + 1..].trim_start();
    if !(after.is_empty() || after.starts_with('#')) {
        return Err(DotenvError {
            line,
            message: "unexpected characters after quoted value".to_owned(),
        });
    }
    if quote == '"' {
        parse_double_quoted(inner, line)
    } else {
        Ok(inner.to_owned())
    }
}

fn find_closing_quote(body: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (index, ch) in body.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote == '"' && ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            return Some(index);
        }
    }
    None
}

fn parse_double_quoted(value: &str, line: usize) -> Result<String, DotenvError> {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let Some(escaped) = chars.next() else {
            return Err(DotenvError {
                line,
                message: "trailing escape in quoted value".to_owned(),
            });
        };
        match escaped {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

fn strip_inline_comment(value: &str) -> &str {
    if let Some((index, _)) = value.match_indices(" #").next() {
        &value[..index]
    } else {
        value
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DotenvError {
    line: usize,
    message: String,
}

impl std::fmt::Display for DotenvError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for DotenvError {}

#[cfg(test)]
mod tests {
    use super::{EnvFileRequired, EnvFileSource, parse_dotenv};

    #[test]
    fn parses_dotenv_lines_and_quotes() {
        let parsed = parse_dotenv(
            r#"
            # skipped
            export A=one
            B=two # comment
            C="three\nfour"
            D='five # not comment'
            "#,
        )
        .expect("dotenv parses");

        assert_eq!(parsed.get("A").map(String::as_str), Some("one"));
        assert_eq!(parsed.get("B").map(String::as_str), Some("two"));
        assert_eq!(parsed.get("C").map(String::as_str), Some("three\nfour"));
        assert_eq!(
            parsed.get("D").map(String::as_str),
            Some("five # not comment")
        );
    }

    #[test]
    fn quoted_values_accept_trailing_comments() {
        let parsed = parse_dotenv(
            r##"
            A="x" # c
            B='y'   # c
            C="z"
            "##,
        )
        .expect("dotenv parses");

        assert_eq!(parsed.get("A").map(String::as_str), Some("x"));
        assert_eq!(parsed.get("B").map(String::as_str), Some("y"));
        assert_eq!(parsed.get("C").map(String::as_str), Some("z"));
    }

    #[test]
    fn unterminated_or_trailing_junk_quotes_stay_fatal() {
        assert!(parse_dotenv("A=\"x").is_err());
        assert!(parse_dotenv("A='x").is_err());
        assert!(parse_dotenv("A=\"x\" junk").is_err());
    }

    #[test]
    fn merges_env_files_in_order_and_later_wins() {
        let (merged, issues) = super::merge_env_files(&[
            EnvFileSource {
                path: "a.env".to_owned(),
                required: EnvFileRequired::Required,
                contents: Some("A=one\nB=old\n".to_owned()),
            },
            EnvFileSource {
                path: "b.env".to_owned(),
                required: EnvFileRequired::Required,
                contents: Some("B=new\n".to_owned()),
            },
        ]);

        assert!(issues.is_empty());
        assert_eq!(merged.get("A").map(String::as_str), Some("one"));
        assert_eq!(merged.get("B").map(String::as_str), Some("new"));
    }

    #[test]
    fn optional_missing_env_file_is_not_an_issue() {
        let (_merged, issues) = super::merge_env_files(&[EnvFileSource {
            path: "missing.env".to_owned(),
            required: EnvFileRequired::Optional,
            contents: None,
        }]);

        assert!(issues.is_empty());
    }
}
