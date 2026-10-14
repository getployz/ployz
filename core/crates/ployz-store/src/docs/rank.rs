//! BM25 over the docs sections, one index per field (title, heading, body),
//! each normalized on its own length and summed with the field's weight.

use std::collections::HashMap;

use super::split::Section;

const K1: f32 = 1.2;
const B: f32 = 0.75;

/// Words that carry no topic: question words, pronouns, glue.
const STOP: &[&str] = &[
    "a", "an", "the", "i", "my", "me", "we", "our", "you", "your", "is", "are", "was", "be", "do",
    "does", "did", "how", "what", "why", "when", "where", "which", "who", "can", "could", "should",
    "would", "to", "of", "in", "on", "for", "and", "or", "it", "its", "this", "that", "with",
    "from", "at", "by", "as", "not", "no", "get", "use", "using", "isn", "t", "s", "there", "if",
    "into", "up", "want", "need",
];

/// A reader's word, and the words the docs use for it.
const SYNONYMS: &[(&str, &str)] = &[
    ("env", "variable"),
    ("var", "variable"),
    ("vars", "variable"),
    ("secret", "sealed"),
    ("secrets", "sealed"),
    ("ssl", "https certificate"),
    ("tls", "https certificate"),
    ("cert", "certificate"),
    ("autoscale", "replicas"),
    ("scale", "replicas"),
    ("backup", "back up"),
    ("rollback", "roll back"),
    ("price", "cost"),
    ("pricing", "cost"),
];

/// Each field's weight: a match in a heading says most about a section.
/// Where a field's text sits in a section.
type FieldText = fn(&Section) -> &str;

const FIELDS: [(FieldText, f32); 3] = [
    (|section| section.title, 2.0),
    (|section| &section.heading, 3.0),
    (|section| &section.text, 1.0),
];

pub(crate) struct Ranker {
    count: usize,
    fields: Vec<Field>,
}

/// One field's postings: per term, each section it appears in with the
/// term's count there and the section's length norm.
struct Field {
    weight: f32,
    postings: HashMap<String, Vec<Posting>>,
}

struct Posting {
    section: usize,
    count: f32,
    norm: f32,
}

impl Ranker {
    pub(crate) fn new(sections: &[Section]) -> Self {
        let fields = FIELDS
            .iter()
            .map(|&(text, weight)| Field::new(sections, text, weight))
            .collect();
        Self {
            count: sections.len(),
            fields,
        }
    }

    /// Every section that matches a term, best first, with its score.
    pub(crate) fn rank(&self, terms: &[String]) -> Vec<(usize, f32)> {
        let total = self.count as f32;
        let mut scores = vec![0.0_f32; self.count];
        for field in &self.fields {
            for postings in terms.iter().filter_map(|term| field.postings.get(term)) {
                let found = postings.len() as f32;
                let idf = ((total - found + 0.5) / (found + 0.5) + 1.0).ln();
                for posting in postings {
                    if let Some(score) = scores.get_mut(posting.section) {
                        *score += field.weight * idf * posting.count * (K1 + 1.0)
                            / (posting.count + posting.norm);
                    }
                }
            }
        }
        let mut ranked = scores
            .into_iter()
            .enumerate()
            .filter(|(_, score)| *score > 0.0)
            .collect::<Vec<_>>();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        ranked
    }
}

impl Field {
    fn new(sections: &[Section], text: FieldText, weight: f32) -> Self {
        let mut counts = HashMap::<String, Vec<(usize, f32)>>::new();
        let mut lengths = Vec::with_capacity(sections.len());
        for (index, section) in sections.iter().enumerate() {
            let mut here = HashMap::<String, f32>::new();
            for term in terms(text(section)) {
                *here.entry(term).or_default() += 1.0;
            }
            lengths.push(here.values().sum::<f32>());
            for (term, count) in here {
                counts.entry(term).or_default().push((index, count));
            }
        }
        let average = (lengths.iter().sum::<f32>() / lengths.len().max(1) as f32).max(1.0);
        let postings = counts
            .into_iter()
            .map(|(term, found)| {
                let postings = found
                    .into_iter()
                    .map(|(section, count)| {
                        let length = lengths.get(section).copied().unwrap_or(average);
                        Posting {
                            section,
                            count,
                            norm: K1 * (1.0 - B + B * length / average),
                        }
                    })
                    .collect();
                (term, postings)
            })
            .collect();
        Self { weight, postings }
    }
}

/// A query's terms, with the docs' words for any reader's word it uses.
pub(crate) fn query_terms(query: &str) -> Vec<String> {
    let mut terms = terms(query).collect::<Vec<_>>();
    for (_, word) in words(query) {
        let word = word.to_lowercase();
        if let Some((_, docs_words)) = SYNONYMS.iter().find(|(reader, _)| *reader == word) {
            terms.extend(self::terms(docs_words));
        }
    }
    terms
}

/// The searchable terms in `text`, in order.
pub(crate) fn terms(text: &str) -> impl Iterator<Item = String> {
    words(text).filter_map(|(_, word)| term(word))
}

/// A word as a term: lowercased and stemmed, or nothing for a stop word.
pub(crate) fn term(word: &str) -> Option<String> {
    let word = word.to_lowercase();
    (!STOP.contains(&word.as_str())).then(|| stem(&word))
}

/// Each run of letters and digits in `text`, with its byte offset.
pub(crate) fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let separator = |c: char| !c.is_alphanumeric();
    text.split_inclusive(separator)
        .scan(0, move |offset, piece| {
            let at = *offset;
            *offset += piece.len();
            Some((at, piece.trim_end_matches(separator)))
        })
        .filter(|(_, word)| !word.is_empty())
}

/// A light English stemmer: plurals and the common verb endings.
fn stem(word: &str) -> String {
    if let Some(stem) = word.strip_suffix("ies").filter(|stem| stem.len() > 1) {
        return format!("{stem}y");
    }
    let plural = !["ss", "us", "is"]
        .iter()
        .any(|ending| word.ends_with(ending));
    word.strip_suffix("ing")
        .or_else(|| word.strip_suffix("ed"))
        .or_else(|| word.strip_suffix('s').filter(|_| plural))
        .filter(|stem| stem.len() > 2)
        .unwrap_or(word)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_plurals_and_verb_endings() {
        let stems = [
            "replicas",
            "policies",
            "deploying",
            "crashed",
            "class",
            "status",
            "bus",
            "is",
        ]
        .map(stem);
        assert_eq!(
            stems,
            [
                "replica", "policy", "deploy", "crash", "class", "status", "bus", "is"
            ]
        );
    }

    #[test]
    fn query_terms_drop_stop_words_and_add_synonyms() {
        assert_eq!(
            query_terms("How do I set env vars?"),
            ["set", "env", "var", "variable", "variable"]
        );
    }

    #[test]
    fn words_carry_byte_offsets() {
        let found = words("café… on a_b").collect::<Vec<_>>();
        assert_eq!(found, [(0, "café"), (9, "on"), (12, "a"), (14, "b")]);
    }
}
