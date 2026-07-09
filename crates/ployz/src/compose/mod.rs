mod diagnostics;
mod env_files;
mod interpolate;
mod model;
mod translate;

use std::collections::BTreeMap;
use std::path::Path;

use ployz_core::deploy::DeployServiceSpec;
use ployz_core::ids::NamespaceId;
use serde_yaml::Value;

pub use diagnostics::{RenderedWarning, UnsupportedFieldMode};

use self::diagnostics::{ComposeDiagnostics, ComposeFinding, ComposePath, KnownUnsupported};
use self::interpolate::{InterpolationFindingKind, apply_merge, interpolate_value};
use self::model::{ComposeDocument, ComposeService};
use self::translate::{ServiceTranslateInput, classify_service};
use crate::commands::{PloyzctlCliError, cli_error};

pub struct ComposeInput<'a> {
    pub source: &'a str,
    pub base_dir: &'a Path,
    pub interpolation_env: BTreeMap<String, String>,
    pub namespace_override: Option<NamespaceId>,
    pub mode: UnsupportedFieldMode,
}

pub fn parse_deploy_file(
    input: ComposeInput<'_>,
) -> Result<(ParsedComposeDeploy, Vec<RenderedWarning>), PloyzctlCliError> {
    let ComposeInput {
        source,
        base_dir,
        interpolation_env,
        namespace_override,
        mode,
    } = input;
    let mut value: Value = serde_yaml::from_str(source)
        .map_err(|error| cli_error(format!("invalid compose YAML: {error}")))?;
    apply_merge(&mut value);
    let mut findings = interpolation_findings(interpolate_value(&mut value, &interpolation_env));
    let compose: ComposeDocument = serde_yaml::from_value(value)
        .map_err(|error| cli_error(format!("invalid compose document: {error}")))?;
    let ComposeDocument {
        name,
        services,
        version,
        volumes,
        networks,
        secrets,
        configs,
        unrecognized,
    } = compose;
    if version.is_some() {
        findings.push(ComposeFinding::advisory(
            ComposePath::root().field("version"),
            "version is obsolete compose metadata; ignored",
        ));
    }
    let top_level_unsupported = [
        ("volumes", volumes, KnownUnsupported::TopLevelVolumes),
        ("networks", networks, KnownUnsupported::TopLevelNetworks),
        ("secrets", secrets, KnownUnsupported::Secrets),
        ("configs", configs, KnownUnsupported::Configs),
    ];
    for (field, value, feature) in top_level_unsupported {
        if value.is_some() {
            findings.push(ComposeFinding::unsupported(
                ComposePath::root().field(field),
                feature,
            ));
        }
    }
    for (key, _value) in unrecognized {
        findings.push(ComposeFinding::unknown(ComposePath::root().field(&key)));
    }
    let namespace_id = match namespace_override {
        Some(namespace_id) => Some(namespace_id),
        None => match name {
            Some(name) => match NamespaceId::try_new(name) {
                Ok(namespace_id) => Some(namespace_id),
                Err(error) => {
                    findings.push(ComposeFinding::invalid(
                        ComposePath::root().field("name"),
                        error,
                    ));
                    None
                }
            },
            None => {
                findings.push(ComposeFinding::invalid(
                    ComposePath::root().field("name"),
                    "compose file needs top-level name or deploy -n NAMESPACE",
                ));
                None
            }
        },
    };
    let mut deploy_services = Vec::new();
    match services {
        Some(services) if services.is_empty() => findings.push(ComposeFinding::invalid(
            ComposePath::root().field("services"),
            "compose file must define at least one service",
        )),
        Some(services) => {
            for (name, value) in services {
                match serde_yaml::from_value::<ComposeService>(value) {
                    Ok(service) => {
                        let (mut service_findings, deploy_service) =
                            classify_service(ServiceTranslateInput {
                                name,
                                service,
                                base_dir,
                                resolution_env: &interpolation_env,
                            });
                        findings.append(&mut service_findings);
                        if let Some(deploy_service) = deploy_service {
                            deploy_services.push(deploy_service);
                        }
                    }
                    Err(error) => findings.push(ComposeFinding::invalid(
                        ComposePath::root().field("services").field(&name),
                        error,
                    )),
                }
            }
        }
        None => findings.push(ComposeFinding::invalid(
            ComposePath::root().field("services"),
            "compose file must define services",
        )),
    }

    let diagnostics = ComposeDiagnostics::new(findings);
    let warnings =
        diagnostics
            .resolve(mode)
            .map_err(|rejection| PloyzctlCliError::ComposeRejected {
                rendered: rejection.rendered,
            })?;
    let Some(namespace_id) = namespace_id else {
        return Err(cli_error(
            "compose diagnostics accepted a file without a namespace",
        ));
    };
    if deploy_services.is_empty() {
        return Err(cli_error(
            "compose file must define at least one valid service",
        ));
    }
    Ok((
        ParsedComposeDeploy {
            namespace_id,
            services: deploy_services,
        },
        warnings,
    ))
}

fn interpolation_findings(
    findings: Vec<self::interpolate::InterpolationFinding>,
) -> Vec<ComposeFinding> {
    findings
        .into_iter()
        .map(|finding| {
            let path = path_from_interpolation(&finding.path);
            match finding.kind {
                InterpolationFindingKind::MissingVariable { name } => ComposeFinding::advisory(
                    path,
                    format!("interpolation variable {name} is unset; substituted empty string"),
                ),
                InterpolationFindingKind::RequiredVariable { name, message } => {
                    ComposeFinding::invalid(
                        path,
                        format!("interpolation variable {name} is required: {message}"),
                    )
                }
                InterpolationFindingKind::InvalidExpression { message } => {
                    ComposeFinding::invalid(path, message)
                }
            }
        })
        .collect()
}

fn path_from_interpolation(path: &str) -> ComposePath {
    if path.is_empty() {
        return ComposePath::root();
    }
    path.split('.')
        .fold(ComposePath::root(), |path, field| path.field(field))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedComposeDeploy {
    pub namespace_id: NamespaceId,
    pub services: Vec<DeployServiceSpec>,
}

pub(crate) fn interpolation_env(
    base_dir: &Path,
) -> Result<BTreeMap<String, String>, PloyzctlCliError> {
    let mut values = match std::fs::read_to_string(base_dir.join(".env")) {
        Ok(contents) => env_files::parse_dotenv(&contents).map_err(|error| {
            cli_error(format!(
                "invalid value in {}: {error}",
                base_dir.join(".env").display()
            ))
        })?,
        Err(_error) => BTreeMap::new(),
    };
    values.extend(std::env::vars());
    Ok(values)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use ployz_core::deploy::ReplicaCount;

    use super::{ComposeInput, UnsupportedFieldMode, parse_deploy_file};

    #[test]
    fn strict_lists_all_service_findings() {
        let error = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                healthcheck: {}
                mystery: true
              worker:
                command: []
            "#,
            base_dir: Path::new("."),
            interpolation_env: BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect_err("strict compose rejects");

        let rendered = error.to_string();
        assert!(rendered.contains("services.web.healthcheck"));
        assert!(rendered.contains("services.web.mystery"));
        assert!(rendered.contains("services.worker.image"));
        assert!(rendered.contains("services.worker.command"));
    }

    #[test]
    fn allow_unsupported_returns_warnings_and_request() {
        let (parsed, warnings) = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                healthcheck: {}
            "#,
            base_dir: Path::new("."),
            interpolation_env: BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::AllowUnsupported,
        })
        .expect("allow mode parses");

        assert_eq!(parsed.services.len(), 1);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn standard_ports_reject_even_under_allow_unsupported() {
        let error = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                ports: ["8080:80"]
            "#,
            base_dir: Path::new("."),
            interpolation_env: BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::AllowUnsupported,
        })
        .expect_err("standard ports reject even with --allow-unsupported");

        let rendered = error.to_string();
        assert!(rendered.contains("services.web.ports"));
        assert!(rendered.contains("x-ports"));
    }

    #[test]
    fn interpolates_before_typed_replica_parse() {
        let mut interpolation_env = BTreeMap::new();
        interpolation_env.insert("REPLICAS".to_owned(), "2".to_owned());
        let (parsed, warnings) = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                deploy:
                  replicas: ${REPLICAS}
            "#,
            base_dir: Path::new("."),
            interpolation_env,
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect("compose parses");

        assert!(warnings.is_empty());
        assert_eq!(
            parsed.services.first().expect("one service").replicas,
            ReplicaCount::try_new(2).expect("valid replica count")
        );
    }

    #[test]
    fn null_and_bare_env_entries_resolve_from_interpolation_env() {
        let mut interpolation_env = BTreeMap::new();
        interpolation_env.insert("FROM_MAP".to_owned(), "map-value".to_owned());
        interpolation_env.insert("FROM_LIST".to_owned(), "list-value".to_owned());
        let (parsed, warnings) = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                environment:
                  FROM_MAP:
              worker:
                image: nginx
                environment:
                  - FROM_LIST
            "#,
            base_dir: Path::new("."),
            interpolation_env,
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect("compose parses");

        assert!(warnings.is_empty());
        let env_of = |index: usize| {
            parsed
                .services
                .get(index)
                .expect("service exists")
                .runtime
                .environment
                .iter()
                .map(|(name, value)| (name.as_str().to_owned(), value.as_str().to_owned()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            env_of(0),
            vec![("FROM_MAP".to_owned(), "map-value".to_owned())]
        );
        assert_eq!(
            env_of(1),
            vec![("FROM_LIST".to_owned(), "list-value".to_owned())]
        );
    }

    #[test]
    fn dotenv_only_variable_populates_bare_env_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".env"), "ONLY_IN_DOTENV=from-dotenv\n")
            .expect("write .env");
        let interpolation_env = super::interpolation_env(dir.path()).expect(".env parses");
        assert_eq!(
            interpolation_env.get("ONLY_IN_DOTENV").map(String::as_str),
            Some("from-dotenv")
        );

        let (parsed, _warnings) = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                environment:
                  - ONLY_IN_DOTENV
            "#,
            base_dir: dir.path(),
            interpolation_env,
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect("compose parses");

        let environment = &parsed
            .services
            .first()
            .expect("one service")
            .runtime
            .environment;
        assert_eq!(
            environment
                .iter()
                .next()
                .map(|(name, value)| (name.as_str().to_owned(), value.as_str().to_owned())),
            Some(("ONLY_IN_DOTENV".to_owned(), "from-dotenv".to_owned()))
        );
    }

    #[test]
    fn unset_null_and_bare_env_entries_warn_and_are_omitted() {
        let (parsed, warnings) = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                environment:
                  UNSET_MAP:
              worker:
                image: nginx
                environment:
                  - UNSET_LIST
            "#,
            base_dir: Path::new("."),
            interpolation_env: BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect("advisories are not fatal");

        assert!(
            parsed.services.iter().all(|service| service
                .runtime
                .environment
                .iter()
                .next()
                .is_none())
        );
        let rendered = warnings
            .iter()
            .map(|warning| warning.0.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("services.web.environment.UNSET_MAP"));
        assert!(rendered.contains("services.worker.environment[0]"));
    }

    #[test]
    fn env_file_option_findings_surface_when_environment_parses() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.env"), "A=one\n").expect("write env file");

        let error = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                env_file:
                  - path: a.env
                    mystery: true
            "#,
            base_dir: dir.path(),
            interpolation_env: BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect_err("unknown env_file option rejects in strict mode");

        assert!(
            error
                .to_string()
                .contains("services.web.env_file[0].mystery")
        );
    }

    #[test]
    fn version_is_ignored_with_advisory_and_top_level_features_classify() {
        let error = parse_deploy_file(ComposeInput {
            source: r#"
            version: "3.9"
            name: default
            services:
              web:
                image: nginx
            volumes:
              data: {}
            networks:
              internal: {}
            secrets:
              token: {}
            configs:
              settings: {}
            "#,
            base_dir: Path::new("."),
            interpolation_env: BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect_err("top-level unsupported features reject in strict mode");

        let rendered = error.to_string();
        for line in [
            "volumes  unsupported",
            "networks  unsupported",
            "secrets  unsupported",
            "configs  unsupported",
            "version  advisory",
        ] {
            assert!(rendered.contains(line), "missing {line:?} in {rendered}");
        }

        let (parsed, warnings) = parse_deploy_file(ComposeInput {
            source: r#"
            version: "3.9"
            name: default
            services:
              web:
                image: nginx
            "#,
            base_dir: Path::new("."),
            interpolation_env: BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect("version alone never rejects");
        assert_eq!(parsed.services.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(warnings.first().expect("one warning").0.contains("version"));
    }

    #[test]
    fn rejection_output_is_sorted_by_path() {
        let error = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              zeta:
                image: nginx
                ports: ["8080:80"]
              alpha:
                image: nginx
                mystery: true
                healthcheck: {}
            "#,
            base_dir: Path::new("."),
            interpolation_env: BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect_err("strict compose rejects");

        let rendered = error.to_string();
        let paths = rendered
            .lines()
            .filter_map(|line| line.split("  ").next())
            .filter(|path| path.starts_with("services."))
            .collect::<Vec<_>>();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted);
        assert!(
            paths
                .first()
                .is_some_and(|path| path.starts_with("services.alpha"))
        );
    }

    #[test]
    fn env_file_then_environment_merge_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.env"), "A=file\nB=file\n").expect("write a env");
        std::fs::write(dir.path().join("b.env"), "B=later\nC=file\n").expect("write b env");

        let (parsed, warnings) = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                env_file:
                  - a.env
                  - b.env
                environment:
                  C: map
            "#,
            base_dir: dir.path(),
            interpolation_env: BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect("compose parses");

        assert!(warnings.is_empty());
        let environment = &parsed
            .services
            .first()
            .expect("one service")
            .runtime
            .environment;
        let pairs = environment
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), value.as_str().to_owned()))
            .collect::<Vec<_>>();
        assert_eq!(
            pairs,
            vec![
                ("A".to_owned(), "file".to_owned()),
                ("B".to_owned(), "later".to_owned()),
                ("C".to_owned(), "map".to_owned())
            ]
        );
    }
}
