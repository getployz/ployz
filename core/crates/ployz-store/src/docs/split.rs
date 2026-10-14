//! Splits a docs page into sections at its `##` and `###` headings, as plain
//! text for ranking, and rewrites a page's relative links from the docs root.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};

/// One heading's stretch of a page. The text before the first heading has an
/// empty heading and anchor.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Section {
    pub(crate) path: &'static str,
    pub(crate) title: &'static str,
    /// `H2`, or `H2 > H3` under a third-level heading.
    pub(crate) heading: String,
    /// The heading's GitHub anchor.
    pub(crate) anchor: String,
    pub(crate) text: String,
}

/// A page's front matter `title:` and the Markdown after the front matter.
pub(crate) fn front(source: &str) -> (&str, &str) {
    let Some((front, body)) = source
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
    else {
        return ("", source);
    };
    let title = front
        .lines()
        .find_map(|line| line.strip_prefix("title:"))
        .map_or("", str::trim);
    (title, body)
}

pub(crate) fn split(path: &'static str, source: &'static str) -> Vec<Section> {
    let (title, body) = front(source);
    let mut slugs = Slugs::default();
    let mut sections = Vec::new();
    let mut h2 = String::new();
    let mut section = Section {
        path,
        title,
        heading: String::new(),
        anchor: String::new(),
        text: String::new(),
    };
    let mut fenced = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            let heading = match (line.strip_prefix("## "), line.strip_prefix("### ")) {
                (Some(h2_text), _) => {
                    h2 = plain(h2_text.trim());
                    Some((h2.clone(), slugs.next(&h2)))
                }
                (None, Some(h3_text)) => {
                    let h3 = plain(h3_text.trim());
                    let anchor = slugs.next(&h3);
                    Some((format!("{h2} > {h3}"), anchor))
                }
                (None, None) => None,
            };
            if let Some((heading, anchor)) = heading {
                let next = Section {
                    path,
                    title,
                    heading,
                    anchor,
                    text: String::new(),
                };
                push(&mut sections, std::mem::replace(&mut section, next));
                continue;
            }
            if is_table_rule(line) {
                continue;
            }
            section.text.push_str(&plain(line));
            section.text.push('\n');
            continue;
        }
        section.text.push_str(line);
        section.text.push('\n');
    }
    push(&mut sections, section);
    sections
}

fn push(sections: &mut Vec<Section>, mut section: Section) {
    let trimmed = section.text.trim();
    if trimmed.is_empty() && section.heading.is_empty() {
        return;
    }
    section.text = trimmed.to_owned();
    sections.push(section);
}

/// A table's `|---|---|` row.
fn is_table_rule(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('|') && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// GitHub's heading anchors for one page: a repeated heading gets `-1`, `-2`.
#[derive(Default)]
struct Slugs(HashMap<String, usize>);

impl Slugs {
    fn next(&mut self, heading: &str) -> String {
        let slug = slug(heading);
        let seen = self.0.entry(slug.clone()).or_default();
        let anchor = match *seen {
            0 => slug,
            repeat => format!("{slug}-{repeat}"),
        };
        *seen += 1;
        anchor
    }
}

/// GitHub's slug: lowercase letters, digits, `-` and `_`, spaces as `-`.
pub(crate) fn slug(heading: &str) -> String {
    heading
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// A Markdown line as the words a reader sees: link text without its target,
/// no images, no emphasis stars or quote marker. Code spans keep every
/// character inside them, so `*.example.com` stays whole.
pub(crate) fn plain(line: &str) -> String {
    let line = line
        .trim_start()
        .strip_prefix('>')
        .map_or(line, str::trim_start);
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if c == '`' {
            let body = rest.trim_start_matches('`');
            let fence = "`".repeat(rest.len() - body.len());
            let Some((code, after)) = body.split_once(fence.as_str()) else {
                out.push_str(body);
                break;
            };
            out.push_str(code);
            rest = after;
            continue;
        }
        if let Some((_, after)) = rest.strip_prefix('!').and_then(link) {
            rest = after;
            continue;
        }
        if let Some((text, after)) = link(rest) {
            out.push_str(&plain(text));
            rest = after;
            continue;
        }
        if c != '*' {
            out.push(c);
        }
        rest = rest.get(c.len_utf8()..).unwrap_or_default();
    }
    out
}

/// A `[text](target)` at the start of `rest`: its text and what follows it.
fn link(rest: &str) -> Option<(&str, &str)> {
    let (text, after) = rest.strip_prefix('[')?.split_once(']')?;
    let (_, after) = after.strip_prefix('(')?.split_once(')')?;
    Some((text, after))
}

static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\]\(([^)\s]+)\)").expect("the link pattern compiles"));

/// `body` with each relative link target rewritten from the docs root, so a
/// reader of one page can ask for the page a link names.
pub(crate) fn resolve_links(page: &str, body: &str) -> String {
    LINK.replace_all(body, |captures: &Captures<'_>| {
        let target = captures.get(1).map_or("", |target| target.as_str());
        format!("]({})", resolve(page, target))
    })
    .into_owned()
}

/// `target`, written in `page`, as a path from the docs root.
pub(crate) fn resolve(page: &str, target: &str) -> String {
    if target.contains("://") || target.starts_with('/') || target.starts_with("mailto:") {
        return target.to_owned();
    }
    let (file, anchor) = target
        .split_once('#')
        .map_or((target, None), |(file, anchor)| (file, Some(anchor)));
    let mut parts = page.split('/').collect::<Vec<_>>();
    if !file.is_empty() {
        parts.pop();
        for part in file.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                part => parts.push(part),
            }
        }
    }
    let path = parts.join("/");
    match anchor {
        Some(anchor) => format!("{path}#{anchor}"),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::indexing_slicing, reason = "tests index known sections")]

    use super::*;

    const PAGE: &str = "---\ntitle: Domains\ndescription: x\n---\n\nIntro with a [link](scaling.md#x).\n\n## Add a custom domain\n\nPoint `*.example.com` at **your** server.\n\n### Use Cloudflare\n\nTurn off the proxy.\n\n## FAQ\n\n```bash\n## not a heading\nployz deploy *\n```\n\n## FAQ\n\n| a | b |\n|---|:-:|\n| 1 | 2 |\n";

    #[test]
    fn splits_at_headings_with_github_anchors() {
        let sections = split("services/domains.md", PAGE);
        let shape = sections
            .iter()
            .map(|section| (section.heading.as_str(), section.anchor.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            shape,
            [
                ("", ""),
                ("Add a custom domain", "add-a-custom-domain"),
                ("Add a custom domain > Use Cloudflare", "use-cloudflare"),
                ("FAQ", "faq"),
                ("FAQ", "faq-1"),
            ]
        );
        assert!(sections.iter().all(|section| section.title == "Domains"));
    }

    #[test]
    fn keeps_code_spans_and_fences_whole() {
        let sections = split("services/domains.md", PAGE);
        assert_eq!(sections[1].text, "Point *.example.com at your server.");
        assert_eq!(
            sections[3].text,
            "```bash\n## not a heading\nployz deploy *\n```"
        );
        assert_eq!(sections[4].text, "| a | b |\n| 1 | 2 |");
    }

    #[test]
    fn plain_text_keeps_link_text_and_drops_images() {
        assert_eq!(
            plain("> See ![shot](a.png)[the `ployz *` docs](x.md) *now*"),
            "See the ployz * docs now"
        );
        assert_eq!(plain("an `unclosed span"), "an unclosed span");
        assert_eq!(slug("Upgrade `ployz_d` (v2) now"), "upgrade-ployz_d-v2-now");
    }

    #[test]
    fn resolves_links_from_the_page_folder() {
        assert_eq!(
            resolve("services/domains.md", "scaling.md#x"),
            "services/scaling.md#x"
        );
        assert_eq!(
            resolve("services/domains.md", "../deploy/github.md"),
            "deploy/github.md"
        );
        assert_eq!(
            resolve("services/domains.md", "#faq"),
            "services/domains.md#faq"
        );
        assert_eq!(
            resolve("self-hosting.md", "cli/overview.md"),
            "cli/overview.md"
        );
        assert_eq!(
            resolve("a/b.md", "https://x.dev/y.md"),
            "https://x.dev/y.md"
        );
        assert_eq!(
            resolve_links(
                "services/domains.md",
                "See [scaling](scaling.md#x) and [faq](#faq)."
            ),
            "See [scaling](services/scaling.md#x) and [faq](services/domains.md#faq)."
        );
    }
}
