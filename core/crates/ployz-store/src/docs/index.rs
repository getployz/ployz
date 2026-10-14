//! A tantivy index over the docs sections, held in memory. Title, heading and
//! body are separate fields, scored with BM25 and weighted by where a match
//! sits.

use std::collections::HashSet;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, QueryParser};
use tantivy::schema::{
    Field, IndexRecordOption, STORED, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::{
    Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, StopWordFilter, TextAnalyzer,
};
use tantivy::{Searcher, SingleSegmentIndexWriter, TantivyDocument, Term, doc};

use super::split::Section;

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

const ANALYZER: &str = "docs";

/// Sizes the writer's term table. The docs index to well under this.
const WRITER_MEMORY: usize = 15_000_000;

/// Each field's weight: a match in a heading says most about a section.
const TITLE: f32 = 2.0;
const HEADING: f32 = 3.0;
const BODY: f32 = 1.0;

/// A term the docs never use, this long or longer, matches its spellings one
/// edit away.
const FUZZY_FROM: usize = 5;

pub(crate) struct Index {
    sections: Vec<Section>,
    searcher: Searcher,
    parser: QueryParser,
    /// Each searchable field with its weight.
    fields: [(Field, f32); 3],
    /// The section's position in `sections`.
    at: Field,
    analyzer: TextAnalyzer,
}

impl Index {
    pub(crate) fn new(sections: Vec<Section>) -> Self {
        let mut schema = Schema::builder();
        let text = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(ANALYZER)
                .set_index_option(IndexRecordOption::WithFreqs),
        );
        let title = schema.add_text_field("title", text.clone());
        let heading = schema.add_text_field("heading", text.clone());
        let body = schema.add_text_field("body", text);
        let at = schema.add_u64_field("at", STORED);
        let index = tantivy::Index::create_in_ram(schema.build());
        index.tokenizers().register(ANALYZER, analyzer());

        let mut writer = SingleSegmentIndexWriter::new(index, WRITER_MEMORY)
            .expect("an in-memory index opens a writer");
        for (position, section) in sections.iter().enumerate() {
            writer
                .add_document(doc!(
                    title => section.title,
                    heading => section.heading.as_str(),
                    body => section.text.as_str(),
                    at => position as u64,
                ))
                .expect("an in-memory index takes a document");
        }
        let index = writer.finalize().expect("an in-memory index commits");
        let searcher = index
            .reader()
            .expect("an in-memory index opens a reader")
            .searcher();

        let fields = [(title, TITLE), (heading, HEADING), (body, BODY)];
        let mut parser = QueryParser::for_index(&index, fields.map(|(field, _)| field).to_vec());
        for (field, weight) in fields {
            parser.set_field_boost(field, weight);
        }
        Self {
            sections,
            searcher,
            parser,
            fields,
            at,
            analyzer: analyzer(),
        }
    }

    /// The best `limit` sections for `query`, best first, with their scores.
    pub(crate) fn search(&self, query: &str, limit: usize) -> Vec<(&Section, f32)> {
        // With no term left, the parser would match every section.
        let terms = self.terms(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let (parsed, _) = self.parser.parse_query_lenient(&expand(query));
        let mut clauses = vec![(Occur::Should, parsed)];
        clauses.extend(self.typos(&terms).flat_map(|word| {
            self.fields.map(|(field, weight)| {
                let fuzzy = FuzzyTermQuery::new(Term::from_field_text(field, word), 1, true);
                let query: Box<dyn Query> = Box::new(BoostQuery::new(Box::new(fuzzy), weight));
                (Occur::Should, query)
            })
        }));
        let Ok(top) = self.searcher.search(
            &BooleanQuery::new(clauses),
            &TopDocs::with_limit(limit).order_by_score(),
        ) else {
            return Vec::new();
        };
        top.into_iter()
            .filter_map(|(score, address)| {
                let found = self.searcher.doc::<TantivyDocument>(address).ok()?;
                let at = found.get_first(self.at)?.as_u64()?;
                let section = self.sections.get(usize::try_from(at).ok()?)?;
                Some((section, score))
            })
            .collect()
    }

    /// The terms `query` searches for, synonyms included.
    pub(crate) fn terms(&self, query: &str) -> HashSet<String> {
        let mut analyzer = self.analyzer.clone();
        let expanded = expand(query);
        let mut stream = analyzer.token_stream(&expanded);
        let mut terms = HashSet::new();
        while stream.advance() {
            terms.insert(stream.token().text.clone());
        }
        terms
    }

    /// The byte offset of the first word in `text` that is one of `terms`.
    pub(crate) fn first_match(&self, text: &str, terms: &HashSet<String>) -> Option<usize> {
        let mut analyzer = self.analyzer.clone();
        let mut stream = analyzer.token_stream(text);
        while stream.advance() {
            if terms.contains(&stream.token().text) {
                return Some(stream.token().offset_from);
            }
        }
        None
    }

    /// The terms the docs never use, long enough to be a misspelling
    /// rather than a short word.
    fn typos<'a>(&'a self, terms: &'a HashSet<String>) -> impl Iterator<Item = &'a String> {
        terms.iter().filter(|term| {
            term.chars().count() >= FUZZY_FROM
                && self.fields.iter().all(|&(field, _)| {
                    self.searcher
                        .doc_freq(&Term::from_field_text(field, term))
                        .is_ok_and(|found| found == 0)
                })
        })
    }
}

/// Lowercase words, stop words dropped, stemmed. Index and queries share it.
fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .filter(StopWordFilter::remove(
            STOP.iter().map(|&word| word.to_owned()),
        ))
        .filter(Stemmer::new(Language::English))
        .build()
}

/// The query as plain words, so a reader's punctuation never reads as query
/// syntax, followed by the docs' words for any reader's word it uses.
fn expand(query: &str) -> String {
    let mut words = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    let synonyms = words
        .iter()
        .filter_map(|word| {
            SYNONYMS
                .iter()
                .find(|(reader, _)| reader == word)
                .map(|&(_, docs)| docs)
        })
        .collect::<Vec<_>>();
    words.extend(synonyms.into_iter().map(str::to_owned));
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_drops_punctuation_and_adds_the_docs_words() {
        assert_eq!(
            expand("How do I set env-vars?"),
            "how do i set env vars variable variable"
        );
    }

    #[test]
    fn terms_drop_stop_words_and_stem() {
        let index = Index::new(Vec::new());
        let mut terms = index
            .terms("How do I deploy replicas?")
            .into_iter()
            .collect::<Vec<_>>();
        terms.sort();
        assert_eq!(terms, ["deploy", "replica"]);
    }
}
