//! Tells bot user agents from browsers with isbot's pattern list, vendored by
//! `core/scripts/vendor-isbot.sh`.

use std::{
    borrow::Cow,
    sync::{LazyLock, OnceLock},
};

use regex::{Regex, RegexBuilder, RegexSet, RegexSetBuilder};

const MAX_REASON_BYTES: usize = 64;

static PATTERNS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| patterns(include_str!("bots/patterns.txt")).collect());

static SET: LazyLock<RegexSet> =
    LazyLock::new(|| build(&PATTERNS).expect("vendor-isbot.sh validated every pattern"));

/// Each pattern on its own, compiled the first time it wins a match.
static REGEXES: LazyLock<Vec<OnceLock<Regex>>> =
    LazyLock::new(|| PATTERNS.iter().map(|_| OnceLock::new()).collect());

/// Why a user agent counts as a bot, or `None` for a browser: `empty_ua`, or
/// a label from what the first matching isbot pattern matched.
pub(super) fn reason(user_agent: &str) -> Option<Cow<'static, str>> {
    if user_agent.trim().is_empty() {
        return Some(Cow::Borrowed("empty_ua"));
    }
    let index = SET.matches(user_agent).iter().next()?;
    let pattern = PATTERNS.get(index)?;
    let regex = REGEXES.get(index)?.get_or_init(|| {
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .expect("vendor-isbot.sh validated every pattern")
    });
    let matched = regex.find(user_agent)?;
    Some(Cow::Owned(label(
        user_agent,
        matched.start(),
        matched.end(),
    )))
}

/// The user agent's token around its first token byte in `start..end`,
/// lowercased, trimmed to alphanumerics at both ends and cut to 64 bytes. A
/// token runs over `[A-Za-z0-9._-]`, so `/` stops it before a version. A match
/// with no token byte is `unlabeled`.
fn label(user_agent: &str, start: usize, end: usize) -> String {
    let bytes = user_agent.as_bytes();
    let is_token = |byte: &u8| byte.is_ascii_alphanumeric() || b"._-".contains(byte);
    let Some(seed) = bytes
        .get(start..end)
        .and_then(|matched| matched.iter().position(is_token))
        .map(|offset| start + offset)
    else {
        return "unlabeled".to_owned();
    };
    let first = bytes
        .get(..seed)
        .and_then(|before| before.iter().rposition(|byte| !is_token(byte)))
        .map_or(0, |stop| stop + 1);
    let last = bytes
        .get(seed..)
        .and_then(|after| after.iter().position(|byte| !is_token(byte)))
        .map_or(bytes.len(), |stop| seed + stop);
    let token = user_agent
        .get(first..last)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let trimmed = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let cut = trimmed
        .get(..trimmed.len().min(MAX_REASON_BYTES))
        .unwrap_or_default()
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    if cut.is_empty() {
        "unlabeled".to_owned()
    } else {
        cut.to_owned()
    }
}

fn patterns(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

fn build(patterns: &[&str]) -> Result<RegexSet, regex::Error> {
    RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .dfa_size_limit(1 << 24)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run by `vendor-isbot.sh` against its candidate list through `PLOYZ_BOTS_PATTERNS`.
    #[test]
    fn patterns_compile() {
        let text = match std::env::var("PLOYZ_BOTS_PATTERNS") {
            Ok(path) => std::fs::read_to_string(&path).expect("read PLOYZ_BOTS_PATTERNS"),
            Err(_) => include_str!("bots/patterns.txt").to_owned(),
        };
        let patterns: Vec<&str> = patterns(&text).collect();
        for (index, pattern) in patterns.iter().enumerate() {
            if let Err(error) = regex::RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
            {
                panic!("pattern {index} rejected by the regex crate: {pattern}\n{error}");
            }
        }
        build(&patterns).expect("patterns compile as one set");
    }

    fn agents(text: &str) -> Vec<&str> {
        patterns(text).collect()
    }

    fn flagged<'a>(agents: &[&'a str]) -> Vec<&'a str> {
        agents
            .iter()
            .copied()
            .filter(|ua| reason(ua).is_some())
            .collect()
    }

    fn share(count: usize, total: usize) -> f64 {
        count as f64 / total as f64
    }

    #[test]
    fn catches_isbot_crawlers() {
        let crawlers = agents(include_str!("bots/fixtures/crawlers.txt"));
        let caught = flagged(&crawlers).len();
        assert!(
            share(caught, crawlers.len()) >= 0.99,
            "caught {caught} of {}",
            crawlers.len()
        );
    }

    #[test]
    fn passes_isbot_browsers() {
        let browsers = agents(include_str!("bots/fixtures/browsers.txt"));
        let wrong = flagged(&browsers);
        assert!(
            share(wrong.len(), browsers.len()) <= 0.01,
            "flagged browsers: {wrong:#?}"
        );
    }

    #[test]
    fn sorts_the_hand_labelled_sample() {
        let mut missed_bots = Vec::new();
        let mut flagged_humans = Vec::new();
        for row in agents(include_str!("bots/fixtures/sample.tsv")) {
            let mut fields = row.splitn(3, '\t');
            let label = fields.next().unwrap();
            let ua = fields.nth(1).unwrap_or("");
            match (label, reason(ua)) {
                ("bot", None) => missed_bots.push(ua),
                ("human", Some(_)) => flagged_humans.push(ua),
                ("bot" | "human", _) => {}
                _ => panic!("bad label in {row:?}"),
            }
        }
        assert_eq!(flagged_humans, Vec::<&str>::new());
        assert_eq!(
            missed_bots,
            [
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Lighthouse/12.0",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            ]
        );
    }

    #[test]
    fn empty_user_agent_is_a_bot() {
        assert_eq!(reason(""), Some("empty_ua".into()));
        assert_eq!(reason(" \t "), Some("empty_ua".into()));
    }

    #[test]
    fn labels_a_bot_by_what_its_pattern_matched() {
        let labels: Vec<_> = [
            "Googlebot/2.1 (+http://www.google.com/bot.html)",
            "Mozilla/5.0 (compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm)",
            "curl/8.5.0",
            "python-requests/2.32.3",
            "Mozilla/5.0 (compatible; Daum/4.1; +http://cs.daum.net/faq/15/4118.html)",
        ]
        .into_iter()
        .map(|ua| reason(ua).unwrap())
        .collect();
        assert_eq!(
            labels,
            ["googlebot", "bingbot", "curl", "python-requests", "daum"]
        );
    }

    #[test]
    fn a_label_is_the_trimmed_lowercase_token_around_the_match_cut_to_64_bytes() {
        let whole = |ua: &str| label(ua, 0, ua.len());
        assert_eq!(label("x MegaIndex.ru/2.0 y", 6, 8), "megaindex.ru");
        assert_eq!(whole("-.Some_Bot-."), "some_bot");
        assert_eq!(whole(" /:( "), "unlabeled");
        assert_eq!(whole("._-"), "unlabeled");
        assert_eq!(whole(&"a".repeat(70)), "a".repeat(64));
    }
}
