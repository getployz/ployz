//! Tells bot user agents from browsers with isbot's pattern list, vendored by
//! `core/scripts/vendor-isbot.sh`.

use std::sync::LazyLock;

use regex::{RegexSet, RegexSetBuilder};

const MAX_REASON_BYTES: usize = 64;

static PATTERNS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| patterns(include_str!("bots/patterns.txt")).collect());

static SET: LazyLock<RegexSet> =
    LazyLock::new(|| build(&PATTERNS).expect("vendor-isbot.sh validated every pattern"));

/// Why a user agent counts as a bot, or `None` for a browser: `empty_ua`, or
/// the first matching isbot pattern cut to 64 bytes.
pub(super) fn reason(user_agent: &str) -> Option<&'static str> {
    if user_agent.trim().is_empty() {
        return Some("empty_ua");
    }
    let pattern = PATTERNS[SET.matches(user_agent).iter().next()?];
    Some(&pattern[..pattern.floor_char_boundary(MAX_REASON_BYTES)])
}

fn patterns(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

fn build(patterns: &[&str]) -> Result<RegexSet, regex::Error> {
    RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .size_limit(1 << 28)
        .dfa_size_limit(1 << 28)
        .build()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

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

    fn flagged(agents: &[&str]) -> Vec<String> {
        agents
            .iter()
            .filter(|ua| reason(ua).is_some())
            .map(|ua| ua.to_string())
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
        assert_eq!(reason(""), Some("empty_ua"));
        assert_eq!(reason(" \t "), Some("empty_ua"));
    }

    #[test]
    fn reports_the_first_matching_pattern() {
        assert_eq!(
            reason("Mozilla/5.0 (compatible; Daum/4.1; +http://cs.daum.net/faq/15/4118.html)"),
            Some(" daum[ /]"),
        );
        assert_eq!(reason("curl/8.5.0"), Some("^curl"));
    }

    #[test]
    fn cuts_long_patterns_to_64_bytes() {
        let reason = reason("Googlebot/2.1 (+http://www.google.com/bot.html)").unwrap();
        assert_eq!(
            reason,
            "(?:^|[^/]|(?:^|[^le])/|(?:^|[^e])l/|(?:^|[^n])el/|(?:^|[^n])nel/"
        );
        assert_eq!(reason.len(), MAX_REASON_BYTES);
    }

    #[test]
    fn classifies_cheaply() {
        let mut sample = agents(include_str!("bots/fixtures/browsers.txt"));
        sample.extend(agents(include_str!("bots/fixtures/crawlers.txt")));
        reason("warm");
        let rounds = 5;
        let start = Instant::now();
        for _ in 0..rounds {
            for ua in &sample {
                std::hint::black_box(reason(ua));
            }
        }
        let per_ua = start.elapsed() / (rounds * sample.len()) as u32;
        eprintln!("{} ns per user agent", per_ua.as_nanos());
        assert!(per_ua.as_micros() < 100, "{per_ua:?} per user agent");
    }
}
