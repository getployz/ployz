//! The Ployz user docs, embedded at build time: search them by question, or
//! read one page. The docs tool and `ployz docs` both answer through
//! [`lookup`].

mod index;
mod split;

use std::sync::OnceLock;

use serde::Serialize;

include!(concat!(env!("OUT_DIR"), "/docs_pages.rs"));

/// How many sections a search returns.
pub const RESULTS: usize = 5;

/// About how many characters of a section a result quotes.
const EXCERPT: usize = 200;

/// What a docs query answers: sections for a question, or the page it names.
pub enum Lookup {
    Search(Found),
    Page(Page),
    /// It named a page the docs don't have.
    Missing {
        path: String,
        /// The page it most likely meant.
        closest: Option<&'static str>,
    },
}

#[derive(Serialize)]
pub struct Found {
    pub query: String,
    pub results: Vec<Hit>,
}

#[derive(Serialize)]
pub struct Hit {
    pub path: &'static str,
    pub title: &'static str,
    pub heading: &'static str,
    pub anchor: &'static str,
    pub score: f64,
    pub excerpt: String,
}

#[derive(Serialize)]
pub struct Page {
    pub path: &'static str,
    pub title: &'static str,
    /// The page's Markdown, its relative links rewritten from the docs root.
    pub content: String,
}

/// A query ending in `.md`, with or without an `#anchor`, names a page;
/// anything else is a question.
#[must_use]
pub fn lookup(query: &str) -> Lookup {
    let query = query.trim();
    let named = query.split_once('#').map_or(query, |(path, _)| path);
    if !named.ends_with(".md") {
        return Lookup::Search(search(query));
    }
    let named = named.trim_start_matches("./");
    let suffix = format!("/{named}");
    let mut matches = PAGES
        .iter()
        .filter(|(path, _)| *path == named || path.ends_with(&suffix));
    match (matches.next(), matches.next()) {
        (Some(&(path, source)), None) => Lookup::Page(page(path, source)),
        _ => Lookup::Missing {
            path: named.to_owned(),
            closest: crate::error::did_you_mean(named, pages()),
        },
    }
}

/// Every page's path.
pub fn pages() -> impl Iterator<Item = &'static str> {
    PAGES.iter().map(|(path, _)| *path)
}

fn page(path: &'static str, source: &'static str) -> Page {
    let (title, body) = split::front(source);
    Page {
        path,
        title,
        content: split::resolve_links(path, body.trim()),
    }
}

fn search(query: &str) -> Found {
    let index = index();
    let terms = index.terms(query);
    let results = index
        .search(query, RESULTS)
        .into_iter()
        .map(|(section, score)| Hit {
            path: section.path,
            title: section.title,
            heading: &section.heading,
            anchor: &section.anchor,
            score: (f64::from(score) * 100.0).round() / 100.0,
            excerpt: excerpt(&section.text, index.first_match(&section.text, &terms)),
        })
        .collect();
    Found {
        query: query.to_owned(),
        results,
    }
}

fn index() -> &'static index::Index {
    static INDEX: OnceLock<index::Index> = OnceLock::new();
    INDEX.get_or_init(|| {
        index::Index::new(
            PAGES
                .iter()
                .flat_map(|&(path, source)| split::split(path, source))
                .collect(),
        )
    })
}

/// About [`EXCERPT`] characters of `text` on one line, starting a little
/// before `at`, the first word that matches the query.
fn excerpt(text: &str, at: Option<usize>) -> String {
    let at = at.unwrap_or(0);
    let before = text.get(..at).unwrap_or_default();
    let start = before
        .char_indices()
        .rev()
        .nth(EXCERPT / 3)
        .map_or(0, |(start, _)| start);
    let mut rest = text.get(start..).unwrap_or(text);
    if start > 0 {
        rest = rest
            .split_once(char::is_whitespace)
            .map_or(rest, |(_, rest)| rest);
    }
    let flat = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cut = flat.chars().take(EXCERPT).collect::<String>();
    if cut.len() < flat.len() {
        if let Some(space) = cut.rfind(' ') {
            cut.truncate(space);
        }
        cut.push('…');
    }
    if start > 0 {
        cut.insert(0, '…');
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Questions a member might ask, none of them tuned for, and the
    /// sections that answer them.
    const QUESTIONS: &[(&str, &[&str])] = &[
        ("how do I see logs", &["observe/logs.md#see-your-logs"]),
        ("deploy a monorepo", &["deploy/github.md#deploy-a-monorepo"]),
        ("install the cli", &["cli/overview.md#install-the-cli"]),
        (
            "server shows offline",
            &["troubleshooting/servers.md#a-server-shows-offline"],
        ),
        (
            "build on github actions",
            &["builds/where-builds-run.md#build-on-github-actions"],
        ),
        (
            "mount a config file",
            &["services/configs.md#mount-it-into-a-service"],
        ),
        (
            "healthcheck failing",
            &[
                "troubleshooting/deployments.md#my-running-service-fails-its-healthcheck",
                "services/settings.md#healthcheck",
            ],
        ),
        (
            "cloudflare proxy orange cloud",
            &["troubleshooting/app-not-reachable.md#my-domain-is-behind-cloudflare"],
        ),
        (
            "private docker registry image",
            &[
                "deploy/docker-image.md#use-a-private-image",
                "services/settings.md#registry-credentials",
            ],
        ),
        (
            "upgrade ployz on my server",
            &[
                "servers/manage-servers.md#upgrade-ployz-on-a-server",
                "servers/manage-servers.md#upgrade-one-server",
            ],
        ),
        (
            "add a custom domain",
            &["services/domains.md#add-a-custom-domain"],
        ),
    ];

    #[test]
    fn questions_find_their_section_first() {
        let misses = QUESTIONS
            .iter()
            .filter_map(|(question, gold)| {
                let Lookup::Search(found) = lookup(question) else {
                    return Some(format!("{question}: not a search"));
                };
                let top = found
                    .results
                    .first()
                    .map(|hit| format!("{}#{}", hit.path, hit.anchor))
                    .unwrap_or_default();
                (!gold.contains(&top.as_str())).then(|| format!("{question}: {top}"))
            })
            .collect::<Vec<_>>();
        assert!(misses.is_empty(), "{misses:#?}");
    }

    #[test]
    fn a_result_quotes_the_match() {
        let Lookup::Search(found) = lookup("add a custom domain") else {
            panic!("a question searches");
        };
        assert_eq!(found.results.len(), RESULTS);
        let excerpt = &found.results.first().expect("a result").excerpt;
        assert!(excerpt.to_lowercase().contains("domain"), "{excerpt}");
        assert!(excerpt.chars().count() <= EXCERPT + 2, "{excerpt}");
        assert!(!excerpt.contains('\n'), "{excerpt}");
    }

    #[test]
    fn a_misspelled_question_finds_its_section() {
        let Lookup::Search(found) = lookup("custon domian") else {
            panic!("a question searches");
        };
        let top = found.results.first().expect("a misspelling still matches");
        assert_eq!(
            format!("{}#{}", top.path, top.anchor),
            "services/domains.md#add-a-custom-domain"
        );
    }

    #[test]
    fn a_query_of_stop_words_finds_nothing() {
        let Lookup::Search(found) = lookup("how do I") else {
            panic!("a question searches");
        };
        assert!(found.results.is_empty());
    }

    #[test]
    fn a_page_path_reads_the_page() {
        let Lookup::Page(page) = lookup("services/domains.md#add-a-custom-domain") else {
            panic!("a page path reads the page");
        };
        assert_eq!(page.path, "services/domains.md");
        assert!(!page.content.starts_with("---"));
        assert!(
            !page.content.contains("](../"),
            "links resolve from the docs root"
        );
        let Lookup::Page(short) = lookup("domains.md") else {
            panic!("a unique file name reads its page");
        };
        assert_eq!(short.path, "services/domains.md");
        assert!(matches!(lookup("nope.md"), Lookup::Missing { path, .. } if path == "nope.md"));
    }

    #[test]
    fn the_docs_skip_their_own_style_guide() {
        assert!(pages().all(|path| !matches!(path, "README.md" | "STYLE.md")));
        assert!(pages().any(|path| path == "services/domains.md"));
    }
}
