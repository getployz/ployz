use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::cli::Scenario;
use crate::error::{Error, Result};

pub(crate) struct ScenarioOutcome {
    pub(crate) scenario: Scenario,
    pub(crate) duration: Duration,
    pub(crate) failure: Option<String>,
}

pub(crate) fn write(path: &Path, outcomes: &[ScenarioOutcome]) -> Result<()> {
    let failures = outcomes.iter().filter(|o| o.failure.is_some()).count();
    let total_time = outcomes
        .iter()
        .map(|o| o.duration.as_secs_f64())
        .sum::<f64>();

    let mut xml = String::new();
    let _ = writeln!(&mut xml, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        &mut xml,
        r#"<testsuites name="ployz-e2e" tests="{}" failures="{}" time="{:.3}">"#,
        outcomes.len(),
        failures,
        total_time
    );
    let _ = writeln!(
        &mut xml,
        r#"  <testsuite name="hostexec-e2e" tests="{}" failures="{}" time="{:.3}">"#,
        outcomes.len(),
        failures,
        total_time
    );
    for outcome in outcomes {
        let name = outcome.scenario.as_str();
        let secs = outcome.duration.as_secs_f64();
        match &outcome.failure {
            None => {
                let _ = writeln!(
                    &mut xml,
                    r#"    <testcase classname="hostexec-e2e" name="{name}" time="{secs:.3}"/>"#,
                );
            }
            Some(message) => {
                let escaped = escape_xml(message);
                let _ = writeln!(
                    &mut xml,
                    r#"    <testcase classname="hostexec-e2e" name="{name}" time="{secs:.3}">"#,
                );
                let _ = writeln!(
                    &mut xml,
                    r#"      <failure message="scenario failed"><![CDATA[{escaped}]]></failure>"#,
                );
                let _ = writeln!(&mut xml, r#"    </testcase>"#);
            }
        }
    }
    let _ = writeln!(&mut xml, r#"  </testsuite>"#);
    let _ = writeln!(&mut xml, r#"</testsuites>"#);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::Io(format!(
                "create junit parent dir '{}': {error}",
                parent.display()
            ))
        })?;
    }
    fs::write(path, xml).map_err(|error| {
        Error::Io(format!("write junit file '{}': {error}", path.display()))
    })?;
    Ok(())
}

fn escape_xml(input: &str) -> String {
    input.replace("]]>", "]]]]><![CDATA[>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn writes_pass_and_failure_cases() {
        let tmp = std::env::temp_dir().join(format!(
            "ployz-e2e-junit-test-{}.xml",
            std::process::id()
        ));
        let outcomes = vec![
            ScenarioOutcome {
                scenario: Scenario::SingleNodeInit,
                duration: Duration::from_secs_f64(1.5),
                failure: None,
            },
            ScenarioOutcome {
                scenario: Scenario::DeploySmoke,
                duration: Duration::from_secs_f64(2.25),
                failure: Some("boom\nwith newlines".into()),
            },
        ];
        write(&tmp, &outcomes).expect("write junit");
        let contents = std::fs::read_to_string(&tmp).expect("read junit");
        assert!(contents.contains(r#"tests="2""#));
        assert!(contents.contains(r#"failures="1""#));
        assert!(contents.contains(r#"name="single_node_init""#));
        assert!(contents.contains(r#"name="deploy_smoke""#));
        assert!(contents.contains("<![CDATA[boom\nwith newlines]]>"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn escapes_cdata_terminator_in_failure_message() {
        let tmp = std::env::temp_dir().join(format!(
            "ployz-e2e-junit-cdata-test-{}.xml",
            std::process::id()
        ));
        let outcomes = vec![ScenarioOutcome {
            scenario: Scenario::SingleNodeInit,
            duration: Duration::from_secs_f64(0.1),
            failure: Some("payload with ]]> terminator".into()),
        }];
        write(&tmp, &outcomes).expect("write junit");
        let contents = std::fs::read_to_string(&tmp).expect("read junit");
        assert!(!contents.contains("payload with ]]>"));
        assert!(contents.contains("]]]]><![CDATA[>"));
        let _ = std::fs::remove_file(&tmp);
    }
}
