use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use crate::error::SubjectParseError;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Subject {
    raw: String,
    tokens: Vec<String>,
}

impl Subject {
    pub fn parse(value: impl Into<String>) -> Result<Self, SubjectParseError> {
        let raw = value.into();
        let tokens = parse_tokens(&raw, WildcardPolicy::NoWildcards)?;
        Ok(Self { raw, tokens })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    #[must_use]
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }
}

impl Display for Subject {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for Subject {
    type Err = SubjectParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubjectPattern {
    raw: String,
    tokens: Vec<PatternToken>,
}

impl SubjectPattern {
    pub fn parse(value: impl Into<String>) -> Result<Self, SubjectParseError> {
        let raw = value.into();
        let tokens = parse_tokens(&raw, WildcardPolicy::AllowWildcards)?
            .into_iter()
            .map(|token| match token.as_str() {
                "*" => PatternToken::One,
                ">" => PatternToken::Many,
                _ => PatternToken::Literal(token),
            })
            .collect();
        Ok(Self { raw, tokens })
    }

    #[must_use]
    pub fn matches(&self, subject: &Subject) -> bool {
        let mut subject_index = 0;
        for token in &self.tokens {
            match token {
                PatternToken::Literal(value) => {
                    if subject.tokens.get(subject_index) != Some(value) {
                        return false;
                    }
                    subject_index += 1;
                }
                PatternToken::One => {
                    if subject.tokens.get(subject_index).is_none() {
                        return false;
                    }
                    subject_index += 1;
                }
                PatternToken::Many => return subject_index < subject.tokens.len(),
            }
        }
        subject_index == subject.tokens.len()
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    #[must_use]
    pub fn literal_subject(&self) -> Option<Subject> {
        let mut tokens = Vec::with_capacity(self.tokens.len());
        for token in &self.tokens {
            let PatternToken::Literal(value) = token else {
                return None;
            };
            tokens.push(value.clone());
        }
        Some(Subject {
            raw: self.raw.clone(),
            tokens,
        })
    }

    #[must_use]
    pub fn subsumes(&self, requested: &SubjectPattern) -> bool {
        for (index, token) in self.tokens.iter().enumerate() {
            match token {
                PatternToken::Many => {
                    return requested.tokens.len() > index
                        && prefix_compatible(&self.tokens[..index], requested);
                }
                PatternToken::One => match requested.tokens.get(index) {
                    Some(PatternToken::Literal(_) | PatternToken::One) => {}
                    Some(PatternToken::Many) | None => return false,
                },
                PatternToken::Literal(value) => match requested.tokens.get(index) {
                    Some(PatternToken::Literal(requested)) if requested == value => {}
                    _ => return false,
                },
            }
        }
        requested.tokens.len() == self.tokens.len()
    }

    #[must_use]
    pub fn overlaps(&self, other: &SubjectPattern) -> bool {
        patterns_overlap(&self.tokens, &other.tokens)
    }
}

impl Display for SubjectPattern {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for SubjectPattern {
    type Err = SubjectParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum PatternToken {
    Literal(String),
    One,
    Many,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WildcardPolicy {
    NoWildcards,
    AllowWildcards,
}

fn parse_tokens(
    raw: &str,
    wildcard_policy: WildcardPolicy,
) -> Result<Vec<String>, SubjectParseError> {
    if raw.is_empty() {
        return Err(SubjectParseError::Empty);
    }

    let mut tokens = Vec::new();
    for token in raw.split('.') {
        if token.is_empty() {
            return Err(SubjectParseError::EmptyToken);
        }
        tokens.push(token.to_string());
    }

    match wildcard_policy {
        WildcardPolicy::NoWildcards => {
            if tokens.iter().any(|token| token == "*" || token == ">") {
                return Err(SubjectParseError::WildcardInSubject);
            }
        }
        WildcardPolicy::AllowWildcards => {
            for (index, token) in tokens.iter().enumerate() {
                if token == ">" && index + 1 != tokens.len() {
                    return Err(SubjectParseError::NonTerminalManyWildcard);
                }
            }
        }
    }

    Ok(tokens)
}

fn prefix_compatible(prefix: &[PatternToken], requested: &SubjectPattern) -> bool {
    for (index, token) in prefix.iter().enumerate() {
        match (token, requested.tokens.get(index)) {
            (PatternToken::Literal(left), Some(PatternToken::Literal(right))) if left == right => {}
            (PatternToken::One, Some(PatternToken::Literal(_) | PatternToken::One)) => {}
            _ => return false,
        }
    }
    true
}

fn patterns_overlap(left: &[PatternToken], right: &[PatternToken]) -> bool {
    match (left.split_first(), right.split_first()) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some((PatternToken::Many, _)), Some(_)) => true,
        (Some(_), Some((PatternToken::Many, _))) => true,
        (
            Some((PatternToken::One, left_tail)),
            Some((PatternToken::Literal(_) | PatternToken::One, right_tail)),
        ) => patterns_overlap(left_tail, right_tail),
        (
            Some((PatternToken::Literal(left), left_tail)),
            Some((PatternToken::Literal(right), right_tail)),
        ) if left == right => patterns_overlap(left_tail, right_tail),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{Subject, SubjectPattern};
    use crate::SubjectParseError;

    #[test]
    fn wildcard_matches_one_token() {
        let subject = Subject::parse("node.alpha.status").expect("subject parses");
        let pattern = SubjectPattern::parse("node.*.status").expect("pattern parses");

        assert!(pattern.matches(&subject));
    }

    #[test]
    fn terminal_wildcard_matches_many_tokens() {
        let subject = Subject::parse("node.alpha.status").expect("subject parses");
        let pattern = SubjectPattern::parse("node.>").expect("pattern parses");

        assert!(pattern.matches(&subject));
    }

    #[test]
    fn terminal_wildcard_requires_trailing_token() {
        let subject = Subject::parse("node").expect("subject parses");
        let pattern = SubjectPattern::parse("node.>").expect("pattern parses");

        assert!(!pattern.matches(&subject));
    }

    #[test]
    fn wildcard_does_not_match_missing_tokens() {
        let subject = Subject::parse("node.alpha.status").expect("subject parses");
        let pattern = SubjectPattern::parse("node.*").expect("pattern parses");

        assert!(!pattern.matches(&subject));
    }

    #[test]
    fn rejects_empty_subject() {
        assert_eq!(Subject::parse("").unwrap_err(), SubjectParseError::Empty);
    }

    #[test]
    fn rejects_empty_token() {
        assert_eq!(
            Subject::parse("node..status").unwrap_err(),
            SubjectParseError::EmptyToken
        );
    }

    #[test]
    fn rejects_wildcard_in_concrete_subject() {
        assert_eq!(
            Subject::parse("node.*.status").unwrap_err(),
            SubjectParseError::WildcardInSubject
        );
    }

    #[test]
    fn rejects_non_terminal_many_wildcard() {
        assert_eq!(
            SubjectPattern::parse("node.>.status").unwrap_err(),
            SubjectParseError::NonTerminalManyWildcard
        );
    }

    #[test]
    fn preserves_display_form() {
        let subject = Subject::parse("node.alpha.status").expect("subject parses");

        assert_eq!(subject.to_string(), "node.alpha.status");
    }

    #[test]
    fn terminal_wildcard_subsumes_narrower_pattern() {
        let allowed = SubjectPattern::parse("node.>").expect("pattern parses");
        let requested = SubjectPattern::parse("node.*.status").expect("pattern parses");

        assert!(allowed.subsumes(&requested));
    }

    #[test]
    fn literal_pattern_does_not_subsume_wildcard_pattern() {
        let allowed = SubjectPattern::parse("node.alpha.status").expect("pattern parses");
        let requested = SubjectPattern::parse("node.*.status").expect("pattern parses");

        assert!(!allowed.subsumes(&requested));
    }

    #[test]
    fn broad_wildcard_overlaps_narrow_wildcard() {
        let broad = SubjectPattern::parse("node.>").expect("pattern parses");
        let narrow = SubjectPattern::parse("node.*.capacity").expect("pattern parses");

        assert!(broad.overlaps(&narrow));
        assert!(narrow.overlaps(&broad));
    }
}
