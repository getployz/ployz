use super::{ComposeError, ComposeProject};
use ployz_core::RequestedServiceSpec;
use serde_norway::Value;
use std::collections::BTreeSet;

impl ComposeProject {
    pub fn dependency_order(&self) -> Result<Vec<&RequestedServiceSpec>, ComposeError> {
        fn visit<'a>(
            name: &'a str,
            project: &'a ComposeProject,
            visiting: &mut BTreeSet<&'a str>,
            visited: &mut BTreeSet<&'a str>,
            ordered: &mut Vec<&'a RequestedServiceSpec>,
        ) -> Result<(), ComposeError> {
            if visited.contains(name) {
                return Ok(());
            }
            if !visiting.insert(name) {
                return Err(invalid(format!("dependency cycle at service '{name}'")));
            }
            for dependency in project.dependencies.get(name).into_iter().flatten() {
                visit(
                    dependency.service.as_str(),
                    project,
                    visiting,
                    visited,
                    ordered,
                )?;
            }
            visiting.remove(name);
            visited.insert(name);
            ordered.push(
                project
                    .services
                    .get(name)
                    .ok_or_else(|| invalid(format!("undefined service '{name}'")))?,
            );
            Ok(())
        }

        let mut ordered = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for name in self.services.keys() {
            visit(name, self, &mut visiting, &mut visited, &mut ordered)?;
        }
        Ok(ordered)
    }
}

pub(super) fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Null | Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => None,
    }
}

pub(super) fn bytes_u64(value: &Value) -> Option<u64> {
    let value = scalar(value)?;
    if let Ok(value) = value.parse() {
        return Some(value);
    }
    let split = value.find(|character: char| !character.is_ascii_digit() && character != '.')?;
    let amount = value[..split].parse::<f64>().ok()?;
    let unit = value[split..].to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        _ => return None,
    };
    Some((amount * f64::from(multiplier)) as u64)
}

pub(crate) fn duration_millis(value: Option<&str>) -> Result<Option<u64>, ComposeError> {
    let Some(mut remaining) = value else {
        return Ok(None);
    };
    let mut total = 0.0;
    while !remaining.is_empty() {
        let split = remaining
            .find(|character: char| !character.is_ascii_digit() && character != '.')
            .ok_or_else(|| invalid(format!("invalid duration '{remaining}'")))?;
        let amount = remaining[..split]
            .parse::<f64>()
            .map_err(|_| invalid(format!("invalid duration '{remaining}'")))?;
        remaining = &remaining[split..];
        let unit_len = remaining
            .find(|character: char| character.is_ascii_digit() || character == '.')
            .unwrap_or(remaining.len());
        let unit = &remaining[..unit_len];
        total += amount
            * match unit {
                "ns" => 0.000_001,
                "us" | "µs" => 0.001,
                "ms" => 1.0,
                "s" => 1_000.0,
                "m" => 60_000.0,
                "h" => 3_600_000.0,
                _ => return Err(invalid(format!("invalid duration unit '{unit}'"))),
            };
        remaining = &remaining[unit_len..];
    }
    Ok(Some(total as u64))
}

pub(super) fn invalid(error: impl std::fmt::Display) -> ComposeError {
    ComposeError::Invalid(error.to_string())
}
