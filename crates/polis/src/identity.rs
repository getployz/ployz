//! Product-neutral identity values used by Polis primitives.

use std::fmt;

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IrohEndpointId(String);

impl IrohEndpointId {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn redacted(&self) -> String {
        let value = self.as_str();
        if value.len() <= 12 {
            return "<redacted-endpoint>".to_string();
        }
        format!("{}...{}", &value[..6], &value[value.len() - 6..])
    }
}

impl fmt::Display for IrohEndpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn parse_non_empty<T>(value: impl Into<String>, build: impl FnOnce(String) -> T) -> Result<T> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(Error::MalformedPayload);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_endpoint_id_is_malformed() {
        assert_eq!(IrohEndpointId::parse(""), Err(Error::MalformedPayload));
    }

    #[test]
    fn endpoint_id_redaction_keeps_edges_only() {
        let endpoint = IrohEndpointId::parse("endpoint-1234567890").expect("endpoint");

        assert_eq!(endpoint.redacted(), "endpoi...567890");
    }
}
