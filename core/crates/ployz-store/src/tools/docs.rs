//! The tool that reads the Ployz user docs, built into this binary.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Stop, Tool};
use super::refusal::Refusal;
use crate::docs::{self, Lookup};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Docs {
    /// A question, or a page path ending in .md.
    query: String,
}

impl Tool for Docs {
    const NAME: &'static str = "docs";
    const DESCRIPTION: &'static str = concat!(
        "Search the Ployz docs, or read a page by path. Pages: ",
        include_str!(concat!(env!("OUT_DIR"), "/docs_contents.txt")),
        "."
    );
    const READ_ONLY: bool = true;

    fn example() -> Value {
        json!({ "query": "add a custom domain" })
    }

    fn plan(self, _cx: &mut Cx<'_>) -> Result<Value, Stop> {
        match docs::lookup(&self.query) {
            Lookup::Search(found) => Ok(json!(found)),
            Lookup::Page(page) => Ok(json!(page)),
            Lookup::Missing { path, closest } => {
                Err(Refusal::not_found(format!("No docs page {path}"))
                    .with_did_you_mean(closest)
                    .with_valid(docs::pages())
                    .into())
            }
        }
    }
}
