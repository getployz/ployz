//! Sealed values a Build reads: kept sealed in private material, shaped for its
//! recipe, and opened only by the Builder.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use ployz_core::{DockerfileFacts, EnvValue, OpenedSealed, RequestedServiceSpec, SealedRecord};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{BuildInputs, Error, Recipe, invalid};

/// What a Build's recipe gets of the Sealed values its Service reads. Empty when
/// it reads none, which leaves the Build as it always was.
#[derive(Default)]
pub(super) struct Sealed {
    /// Each value by fingerprint, for a Railpack recipe.
    pub(super) fingerprints: BTreeMap<String, String>,
    /// The build arg a Dockerfile recipe gets: one hash over every fingerprint.
    pub(super) args: BTreeMap<String, String>,
    /// The secrets a Dockerfile recipe mounts, by name.
    pub(super) secrets: BTreeMap<String, CapturedSecret>,
    /// The names a Dockerfile declares as `ARG`, which the Builder passes as build args.
    pub(super) declared: Vec<String>,
    /// The names, for the Build's record.
    pub(super) names: Vec<String>,
}

/// Keep the Sealed values the captured `recipe` reads in private upload
/// material and shape them for it. Railpack shows the build every variable;
/// a Dockerfile reads what [`dockerfile_reads`] finds. A Build that reads
/// none captures as it did before Builders opened them, so any Builder runs
/// it. Only a capture with the values `opened` may seal a Railpack setting,
/// which Railpack then plans with.
///
/// # Errors
/// Fails without the Deploy's Environment to seal for, when the captured
/// Dockerfile or the private file can't be read or written, or when `recipe`
/// would record a value.
pub(super) fn capture(
    service: &RequestedServiceSpec,
    recipe: &Recipe,
    inputs: &BuildInputs,
    opened: Option<&OpenedSealed>,
) -> Result<Sealed, Error> {
    let mut sealed = Sealed::default();
    let Some(record) = SealedRecord::of(service).map_err(|error| invalid(error.to_string()))?
    else {
        return Ok(sealed);
    };
    let record = match recipe {
        Recipe::Railpack { .. } => record,
        Recipe::Dockerfile(dockerfile) => {
            let (args, every) = dockerfile_reads(dockerfile, service)?;
            sealed.declared = record
                .keys()
                .filter(|name| args.contains(*name))
                .map(str::to_owned)
                .collect();
            let Some(record) = record
                .only(|name| every || args.contains(name) || name.starts_with(GIT_AUTH_SECRETS))
            else {
                return Ok(sealed);
            };
            record
        }
    };
    inputs.sealed(&record)?;
    let values = record.values();
    match recipe {
        Recipe::Railpack { command } => {
            // An explicit build command wins, as it does over a plain value.
            let planned = values
                .iter()
                .filter(|(name, _)| command.is_none() || name.as_str() != "RAILPACK_BUILD_CMD");
            if opened.is_none() {
                refuse_railpack_settings(planned.clone().map(|(name, _)| name))?;
            }
            sealed.fingerprints = planned
                .map(|(name, value)| (name.clone(), fingerprint(value)))
                .collect();
        }
        Recipe::Dockerfile(_) => {
            sealed
                .args
                .insert(SEALED_HASH_ARG.into(), sealed_hash(values));
            sealed.secrets = values
                .keys()
                .map(|name| {
                    let file = ployz_build::sealed_file(name);
                    (name.clone(), CapturedSecret { file })
                })
                .collect();
        }
    }
    sealed.names = record.keys().map(str::to_owned).collect();
    Ok(sealed)
}

/// BuildKit's Git sources read these secrets, by host too, without a mount.
const GIT_AUTH_SECRETS: &str = "GIT_AUTH_";

/// The `ARG`s the captured Dockerfile declares, the same bytes BuildKit reads,
/// and whether it may read every Sealed value. BuildKit resolves a secret
/// mount's id late (quoting, `src`, `$ARG`, `ONBUILD`, continuations), and
/// another frontend reads secrets its own way, so any text that could be a
/// mount or select such a frontend reads them all.
fn dockerfile_reads(
    dockerfile: &Path,
    service: &RequestedServiceSpec,
) -> Result<(BTreeSet<String>, bool), Error> {
    let contents = std::fs::read_to_string(dockerfile)
        .map_err(|error| Error::Io(format!("read captured Dockerfile: {error}")))?;
    // A comment may sit inside a continued instruction, so read with them and without.
    let uncommented: String = contents
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    let mounts = [contents.as_str(), uncommented.as_str()]
        .map(fold)
        .iter()
        .any(|text| text.contains("type=secret") || text.contains("type=$"));
    let folded = fold(&contents);
    let selected = ["syntax=", "syntax:"]
        .iter()
        .flat_map(|key| {
            folded
                .match_indices(key)
                .map(|(at, key)| &folded[at + key.len()..])
        })
        .any(|syntax| !standard_frontend(syntax));
    let variable = service
        .container
        .environment
        .get("BUILDKIT_SYNTAX")
        .is_some_and(|value| {
            !value
                .text()
                .map(fold)
                .is_some_and(|syntax| standard_frontend(&syntax))
        });
    Ok((
        DockerfileFacts::parse(&contents).args,
        mounts || selected || variable,
    ))
}

/// `text` lowercased without whitespace, quotes, escape characters or a BOM.
fn fold(text: &str) -> String {
    text.chars()
        .filter(|character| {
            !character.is_whitespace() && !matches!(character, '"' | '\'' | '\\' | '`' | '\u{feff}')
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether folded `syntax` starts with the stock Dockerfile frontend.
fn standard_frontend(syntax: &str) -> bool {
    let syntax = syntax.strip_prefix("docker.io/").unwrap_or(syntax);
    syntax.starts_with("docker/dockerfile:") || syntax.starts_with("docker/dockerfile@")
}

/// Railpack reads its `RAILPACK_*` settings while it plans, where a Sealed
/// value is an empty placeholder, so sealing one would plan a different Build.
fn refuse_railpack_settings<'name>(
    mut sealed: impl Iterator<Item = &'name String>,
) -> Result<(), Error> {
    match sealed.find(|name| name.starts_with("RAILPACK_")) {
        Some(name) => Err(Error::Sealed(format!(
            "{name} is sealed. Railpack settings can't be sealed."
        ))),
        None => Ok(()),
    }
}

/// What the Builder sees of a Sealed value: a digest of its ciphertext, so a
/// re-sealed value changes the Build's cache key and nothing else does.
fn fingerprint(value: &EnvValue) -> String {
    let encoded = serde_json::to_vec(value).expect("sealed values encode");
    Sha256::digest(encoded)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The build arg a Dockerfile may declare right above the RUN that mounts a
/// Sealed value, so a re-sealed value reruns that step. BuildKit never keys
/// its cache on a secret mount's contents.
const SEALED_HASH_ARG: &str = "PLOYZ_SEALED_HASH";

/// One digest over every Sealed value's fingerprint, the same fingerprints
/// Railpack's `secrets-hash` folds in. Never a plaintext.
fn sealed_hash(sealed: &BTreeMap<String, EnvValue>) -> String {
    let fingerprints = sealed
        .iter()
        .map(|(name, value)| format!("{name}={}\n", fingerprint(value)))
        .collect::<String>();
    Sha256::digest(fingerprints)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// One Sealed value as a Compose secret: the file the Builder opens it into.
#[derive(Serialize)]
pub(super) struct CapturedSecret {
    file: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::{
        BuildSpec,
        tests::{capture_one, intent, recipe},
    };
    use ployz_build::{Output, remote::Definition};
    use ployz_core::DeployIntent;
    use std::fs;

    /// `api` with one Text and one Sealed variable, sealed for a test identity.
    fn sealed_intent(ciphertext: u8) -> (DeployIntent, EnvValue) {
        let mut intent = intent(&["api"]);
        let sealed = EnvValue::Sealed(ployz_core::SealedValue {
            sealed: ployz_core::Ciphertext::from_bytes(vec![ciphertext; 48]),
            scope: ployz_core::SealedScope::Shared,
            key: "NPM_TOKEN".into(),
        });
        let container = &mut intent.target.first_mut().unwrap().container;
        container
            .environment
            .insert("NPM_TOKEN".into(), sealed.clone());
        container
            .environment
            .insert("PLAIN".into(), "visible".into());
        container.sealed_identity = Some(ployz_core::SealedIdentity {
            organization: "org_test".into(),
            environment: uuid::Uuid::from_u128(1),
            service: uuid::Uuid::from_u128(2),
        });
        (intent, sealed)
    }

    fn dockerfile_spec(root: &Path, dockerfile: &str) -> BuildSpec {
        let source = root.join("repository");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("Dockerfile"), dockerfile).unwrap();
        BuildSpec {
            context: source.clone(),
            recipe: Recipe::Dockerfile(source.join("Dockerfile")),
        }
    }

    const MOUNTING_DOCKERFILE: &str =
        "FROM scratch\nRUN --mount=type=secret,id=NPM_TOKEN,env=NPM_TOKEN true\n";

    #[test]
    #[expect(clippy::indexing_slicing, reason = "fixed capture fixture")]
    fn a_sealed_value_rides_sealed_in_private_material_and_mounts_as_a_secret() {
        let root = tempfile::tempdir().unwrap();
        let (intent, sealed) = sealed_intent(7);
        let captured =
            capture_one(&intent, dockerfile_spec(root.path(), MOUNTING_DOCKERFILE)).unwrap();
        let recipe = recipe(&captured);
        let build = &recipe["services"]["api"]["build"];
        assert_eq!(build["args"]["PLAIN"], "visible");
        assert!(build["args"].get("NPM_TOKEN").is_none());
        assert_eq!(
            build["secrets"],
            serde_norway::to_value(["NPM_TOKEN"]).unwrap()
        );
        assert_eq!(
            recipe["secrets"]["NPM_TOKEN"]["file"],
            ployz_build::sealed_file("NPM_TOKEN").to_str().unwrap()
        );
        let ciphertext = serde_json::to_string(&sealed).unwrap();
        let compose = fs::read_to_string(captured.inputs.root().join("compose.yaml")).unwrap();
        assert!(!compose.contains(&ciphertext), "{compose}");
        let private: serde_json::Value = serde_json::from_slice(
            &fs::read(captured.inputs.root().join("private/sealed.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(private["identity"]["organization"], "org_test");
        assert_eq!(
            serde_json::from_value::<EnvValue>(private["values"]["NPM_TOKEN"].clone()).unwrap(),
            sealed
        );
        assert!(private["values"].get("PLAIN").is_none());
        // The Builder opens the value into the named file before validation.
        let definition = Definition {
            targets: vec![captured.target.clone()],
            retained_tags: vec![captured.retained_tag.clone()],
            image_contexts: BTreeMap::new(),
            output: Output::Load,
            no_cache: false,
            pull: false,
        };
        use ployz_build::remote::{SealedFiles, validate_capture};
        assert!(
            validate_capture(captured.inputs.root(), &definition, SealedFiles::Opened).is_err()
        );
        validate_capture(captured.inputs.root(), &definition, SealedFiles::Pending).unwrap();
        let opened = captured.inputs.root().join("private/sealed");
        fs::create_dir(&opened).unwrap();
        fs::write(opened.join("NPM_TOKEN"), "opened").unwrap();
        validate_capture(captured.inputs.root(), &definition, SealedFiles::Opened).unwrap();
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "fixed capture fixture")]
    fn a_sealed_part_of_a_template_is_carried_whole() {
        let root = tempfile::tempdir().unwrap();
        let (mut intent, sealed) = sealed_intent(7);
        let EnvValue::Sealed(part) = sealed else {
            unreachable!()
        };
        let template = EnvValue::Template(ployz_core::Template {
            template: vec![
                ployz_core::EnvPart::Text("https://registry.test/?token=".into()),
                ployz_core::EnvPart::Sealed(part),
            ],
        });
        let container = &mut intent.target.first_mut().unwrap().container;
        container
            .environment
            .insert("NPM_TOKEN".into(), template.clone());
        let captured =
            capture_one(&intent, dockerfile_spec(root.path(), MOUNTING_DOCKERFILE)).unwrap();
        let private: serde_json::Value = serde_json::from_slice(
            &fs::read(captured.inputs.root().join("private/sealed.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_value::<EnvValue>(private["values"]["NPM_TOKEN"].clone()).unwrap(),
            template
        );
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "fixed capture fixture")]
    fn a_railpack_build_names_sealed_values_by_fingerprint_only() {
        let root = tempfile::tempdir().unwrap();
        let fingerprints = [7, 8].map(|byte| {
            let (intent, sealed) = sealed_intent(byte);
            let captured = capture_one(
                &intent,
                BuildSpec {
                    context: root.path().to_owned(),
                    recipe: Recipe::Railpack { command: None },
                },
            )
            .unwrap();
            let railpack = fs::read(captured.inputs.root().join("private/railpack.json")).unwrap();
            let private: serde_json::Value = serde_json::from_slice(&railpack).unwrap();
            assert_eq!(private[0]["variables"]["PLAIN"], "visible");
            assert!(private[0]["variables"].get("NPM_TOKEN").is_none());
            let ciphertext = serde_json::to_string(&sealed).unwrap();
            assert!(!String::from_utf8(railpack).unwrap().contains(&ciphertext));
            let fingerprint = private[0]["sealed"]["NPM_TOKEN"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(fingerprint.len(), 64, "{fingerprint}");
            assert!(captured.inputs.root().join("private/sealed.json").is_file());
            fingerprint
        });
        assert_ne!(fingerprints[0], fingerprints[1]);
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "fixed capture fixture")]
    fn a_dockerfile_reading_a_sealed_value_gets_one_hash_of_its_fingerprints() {
        let root = tempfile::tempdir().unwrap();
        let hashes = [7, 8].map(|byte| {
            let (intent, sealed) = sealed_intent(byte);
            let captured =
                capture_one(&intent, dockerfile_spec(root.path(), MOUNTING_DOCKERFILE)).unwrap();
            let recipe = recipe(&captured);
            let hash = recipe["services"]["api"]["build"]["args"]["PLOYZ_SEALED_HASH"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(hash.len(), 64, "{hash}");
            let ciphertext = serde_json::to_string(&sealed).unwrap();
            assert!(!hash.contains(&ciphertext));
            assert_ne!(hash, fingerprint(&sealed));
            hash
        });
        assert_ne!(hashes[0], hashes[1]);
        // Railpack keys its own cache through `secrets-hash`.
        let (intent, _) = sealed_intent(7);
        let captured = capture_one(
            &intent,
            BuildSpec {
                context: root.path().to_owned(),
                recipe: Recipe::Railpack { command: None },
            },
        )
        .unwrap();
        let compose = fs::read_to_string(captured.inputs.root().join("compose.yaml")).unwrap();
        let railpack =
            fs::read_to_string(captured.inputs.root().join("private/railpack.json")).unwrap();
        assert!(!compose.contains("PLOYZ_SEALED_HASH"), "{compose}");
        assert!(!railpack.contains("PLOYZ_SEALED_HASH"), "{railpack}");
    }

    #[test]
    fn a_build_without_sealed_values_captures_what_it_always_did() {
        let root = tempfile::tempdir().unwrap();
        let mut intent = intent(&["api"]);
        intent
            .target
            .first_mut()
            .unwrap()
            .container
            .environment
            .insert("PLAIN".into(), "visible".into());
        let captured =
            capture_one(&intent, dockerfile_spec(root.path(), "FROM scratch\n")).unwrap();
        let compose = fs::read_to_string(captured.inputs.root().join("compose.yaml")).unwrap();
        let expected = format!(
            "services:\n  api:\n    image: registry.invalid/api:1\n    build:\n      context: source/0\n      dockerfile: ../1\n      args:\n        PLAIN: visible\n        PORT: '8080'\n      tags:\n      - registry.invalid/api:1\n      - {}\n",
            captured.retained_tag
        );
        assert_eq!(compose, expected);
        assert!(!captured.inputs.root().join("private/sealed.json").exists());
        assert!(
            !captured
                .inputs
                .root()
                .join("private/sealed-args.json")
                .exists()
        );
    }

    /// A Builder that can't open Sealed values builds it, as it did before #3302.
    #[test]
    fn a_dockerfile_that_reads_no_sealed_value_captures_what_it_always_did() {
        let root = tempfile::tempdir().unwrap();
        let (sealed, _) = sealed_intent(7);
        let mut plain = sealed.clone();
        let container = &mut plain.target.first_mut().unwrap().container;
        container.environment.remove("NPM_TOKEN");
        container.sealed_identity = None;
        for dockerfile in [
            "FROM scratch\n",
            "# syntax=docker/dockerfile:1\nFROM scratch\nARG PLAIN\n# NPM_TOKEN is runtime-only\nRUN --mount=type=cache,target=/c true\n",
        ] {
            let [with, without] = [&sealed, &plain].map(|intent| {
                let captured =
                    capture_one(intent, dockerfile_spec(root.path(), dockerfile)).unwrap();
                assert!(captured.sealed.is_empty(), "{dockerfile}");
                assert!(!captured.inputs.root().join("private/sealed.json").exists());
                fs::read_to_string(captured.inputs.root().join("compose.yaml"))
                    .unwrap()
                    .replace(&captured.retained_tag, "retained")
            });
            assert_eq!(with, without);
        }
    }

    fn with_sealed(intent: &mut DeployIntent, name: &str, byte: u8) {
        let value = EnvValue::Sealed(ployz_core::SealedValue {
            sealed: ployz_core::Ciphertext::from_bytes(vec![byte; 48]),
            scope: ployz_core::SealedScope::Shared,
            key: name.into(),
        });
        intent
            .target
            .first_mut()
            .unwrap()
            .container
            .environment
            .insert(name.into(), value);
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "fixed capture fixture")]
    fn a_dockerfile_captures_only_the_sealed_values_it_may_read() {
        let root = tempfile::tempdir().unwrap();
        let (mut intent, _) = sealed_intent(7);
        with_sealed(&mut intent, "RUNTIME_ONLY", 9);
        let every = ["NPM_TOKEN", "RUNTIME_ONLY"].as_slice();
        for (dockerfile, read) in [
            ("FROM scratch\nARG NPM_TOKEN\n", ["NPM_TOKEN"].as_slice()),
            (MOUNTING_DOCKERFILE, every),
            (
                "FROM scratch\nRUN --mount=type=secret,id=OTHER true\n",
                every,
            ),
            (
                "FROM scratch\nRUN --mount=type=secret,src=NPM_TOKEN,target=/n true\n",
                every,
            ),
            (
                "FROM scratch\nRUN --mount=\"type=Secret,id=\\\"NPM_TOKEN\\\"\" true\n",
                every,
            ),
            (
                "FROM scratch\nARG ID=NPM_TOKEN\nRUN --mount=type=secret,id=$ID true\n",
                every,
            ),
            (
                "FROM scratch\nARG T=secret\nRUN --mount=type=$T,id=NPM_TOKEN true\n",
                every,
            ),
            (
                "FROM scratch AS base\nONBUILD RUN --mount=type = secret,id=NPM_TOKEN true\nFROM base\n",
                every,
            ),
            ("# syntax=example.test/frontend\nFROM scratch\n", every),
            (
                "# escape=`\nFROM scratch\nRUN --mount=type=se`\ncret,id=NPM_TOKEN true\n",
                every,
            ),
            (
                "FROM scratch\nRUN --mount=type=se\\\n# a comment\ncret,id=NPM_TOKEN true\n",
                every,
            ),
            (
                "\u{feff}# syntax=example.test/frontend\nFROM scratch\n",
                every,
            ),
            ("// syntax=example.test/frontend\nFROM scratch\n", every),
            ("{\"syntax\": \"example.test/frontend\"}\n", every),
            (
                "# syntax = docker.io/docker/dockerfile:1.7\nFROM scratch\nARG NPM_TOKEN\n",
                ["NPM_TOKEN"].as_slice(),
            ),
        ] {
            let captured = capture_one(&intent, dockerfile_spec(root.path(), dockerfile)).unwrap();
            assert_eq!(captured.sealed, read, "{dockerfile}");
            let private: serde_json::Value = serde_json::from_slice(
                &fs::read(captured.inputs.root().join("private/sealed.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                private["values"].as_object().unwrap().len(),
                read.len(),
                "{dockerfile}"
            );
            let recipe = recipe(&captured);
            assert_eq!(
                recipe["services"]["api"]["build"]["secrets"],
                serde_norway::to_value(read).unwrap(),
                "{dockerfile}"
            );
        }
    }

    #[test]
    fn a_frontend_or_git_source_may_read_a_sealed_value_without_a_mount() {
        let root = tempfile::tempdir().unwrap();
        let plain = "FROM scratch\nADD https://git.test/private.git /src\n";
        let (mut intent, _) = sealed_intent(7);
        with_sealed(&mut intent, "GIT_AUTH_TOKEN", 8);
        with_sealed(&mut intent, "GIT_AUTH_HEADER.git.test", 9);
        let captured = capture_one(&intent, dockerfile_spec(root.path(), plain)).unwrap();
        assert_eq!(
            captured.sealed,
            ["GIT_AUTH_HEADER.git.test", "GIT_AUTH_TOKEN"]
        );
        let (mut intent, _) = sealed_intent(7);
        for (syntax, read) in [
            (EnvValue::from("docker/dockerfile:1"), false),
            (EnvValue::from("example.test/frontend"), true),
        ] {
            intent
                .target
                .first_mut()
                .unwrap()
                .container
                .environment
                .insert("BUILDKIT_SYNTAX".into(), syntax);
            let captured = capture_one(&intent, dockerfile_spec(root.path(), plain)).unwrap();
            assert_eq!(!captured.sealed.is_empty(), read);
        }
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "fixed capture fixture")]
    fn a_dockerfile_declaring_a_sealed_value_as_arg_captures_only_its_name() {
        let root = tempfile::tempdir().unwrap();
        let (intent, _) = sealed_intent(7);
        for dockerfile in [
            "FROM scratch\nARG NPM_TOKEN\n",
            "FROM scratch\nARG NPM_TOKEN=default\n",
            "FROM scratch AS base\narg PLAIN NPM_TOKEN\nFROM scratch\n",
        ] {
            let captured = capture_one(&intent, dockerfile_spec(root.path(), dockerfile)).unwrap();
            let names: BTreeMap<String, Vec<String>> = serde_json::from_slice(
                &fs::read(captured.inputs.root().join("private/sealed-args.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                names,
                BTreeMap::from([("api".into(), vec!["NPM_TOKEN".into()])])
            );
            let recipe = recipe(&captured);
            assert!(
                recipe["services"]["api"]["build"]["args"]
                    .get("NPM_TOKEN")
                    .is_none()
            );
        }
        for dockerfile in [MOUNTING_DOCKERFILE, "FROM scratch\nARG PLAIN\n"] {
            let captured = capture_one(&intent, dockerfile_spec(root.path(), dockerfile)).unwrap();
            assert!(
                !captured
                    .inputs
                    .root()
                    .join("private/sealed-args.json")
                    .exists()
            );
        }
    }

    #[test]
    fn the_arg_check_reads_the_captured_dockerfile_after_the_source_changes() {
        let root = tempfile::tempdir().unwrap();
        let (intent, _) = sealed_intent(7);
        let service = intent.target.first().unwrap();
        for (captured, later, expected) in [
            ("FROM scratch\nARG NPM_TOKEN\n", MOUNTING_DOCKERFILE, true),
            (MOUNTING_DOCKERFILE, "FROM scratch\nARG NPM_TOKEN\n", false),
        ] {
            let spec = dockerfile_spec(root.path(), MOUNTING_DOCKERFILE);
            let Recipe::Dockerfile(source) = &spec.recipe else {
                unreachable!()
            };
            let mut inputs = BuildInputs::new().unwrap();
            fs::write(source, captured).unwrap();
            let file = inputs.dockerfile(source).unwrap();
            fs::write(source, later).unwrap();
            let sealed = capture(service, &Recipe::Dockerfile(file), &inputs, None).unwrap();
            assert_eq!(!sealed.declared.is_empty(), expected);
        }
    }

    #[test]
    fn only_a_sealed_build_needs_to_read_the_captured_dockerfile() {
        let root = tempfile::tempdir().unwrap();
        let missing = Recipe::Dockerfile(root.path().join("missing"));
        let inputs = BuildInputs::new().unwrap();
        let plain = intent(&["api"]);
        capture(plain.target.first().unwrap(), &missing, &inputs, None).unwrap();
        let (sealed, _) = sealed_intent(7);
        assert!(matches!(
            capture(sealed.target.first().unwrap(), &missing, &inputs, None),
            Err(Error::Io(_))
        ));
    }

    #[test]
    fn a_sealed_railpack_setting_is_refused_by_name() {
        let root = tempfile::tempdir().unwrap();
        let (mut intent, _) = sealed_intent(7);
        let setting = EnvValue::Sealed(ployz_core::SealedValue {
            sealed: ployz_core::Ciphertext::from_bytes(vec![9; 48]),
            scope: ployz_core::SealedScope::Shared,
            key: "RAILPACK_BUILD_CMD".into(),
        });
        let ciphertext = serde_json::to_string(&setting).unwrap();
        intent
            .target
            .first_mut()
            .unwrap()
            .container
            .environment
            .insert("RAILPACK_BUILD_CMD".into(), setting);
        let railpack = || BuildSpec {
            context: root.path().to_owned(),
            recipe: Recipe::Railpack { command: None },
        };
        let error = capture_one(&intent, railpack()).err().unwrap().to_string();
        assert_eq!(
            error,
            "RAILPACK_BUILD_CMD is sealed. Railpack settings can't be sealed."
        );
        assert!(!error.contains(&ciphertext));
        assert!(!error.contains("visible"));
        // A Dockerfile never reads Railpack settings, so it mounts the value like any other.
        let dockerfile = "FROM scratch
RUN --mount=type=secret,id=RAILPACK_BUILD_CMD true
";
        capture_one(&intent, dockerfile_spec(root.path(), dockerfile)).unwrap();
        // Only the prefix is refused: another sealed value rides as a fingerprint.
        let (intent, _) = sealed_intent(7);
        capture_one(&intent, railpack()).unwrap();
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "fixed capture fixture")]
    fn a_runner_holding_its_sealed_values_plans_railpack_with_them() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("repository");
        fs::create_dir_all(source.join("config")).unwrap();
        fs::write(
            source.join("config/custom.json"),
            r#"{"exclude": ["drop.log"]}"#,
        )
        .unwrap();
        fs::write(source.join("drop.log"), "excluded").unwrap();
        let (mut intent, _) = sealed_intent(7);
        let setting = EnvValue::Sealed(ployz_core::SealedValue {
            sealed: ployz_core::Ciphertext::from_bytes(vec![9; 48]),
            scope: ployz_core::SealedScope::Shared,
            key: "RAILPACK_CONFIG_FILE".into(),
        });
        intent
            .target
            .first_mut()
            .unwrap()
            .container
            .environment
            .insert("RAILPACK_CONFIG_FILE".into(), setting);
        let opened = OpenedSealed::new(
            BTreeMap::from([
                ("NPM_TOKEN".into(), "marker-sealed-token".into()),
                ("RAILPACK_CONFIG_FILE".into(), "config/custom.json".into()),
            ]),
            Vec::new(),
        );
        let name = intent.target.first().unwrap().name.clone();
        let spec = BuildSpec {
            context: source,
            recipe: Recipe::Railpack { command: None },
        };
        let mut build =
            crate::build::capture(&intent, BTreeMap::from([(name, spec)]), Some(opened)).unwrap();
        let captured = build.targets.pop().unwrap();
        let root = captured.inputs.root();
        let compose: serde_norway::Value =
            serde_norway::from_slice(&fs::read(root.join("compose.yaml")).unwrap()).unwrap();
        let context = root.join(
            compose["services"]["api"]["build"]["context"]
                .as_str()
                .unwrap(),
        );
        assert!(context.join("config/custom.json").exists());
        assert!(!context.join("drop.log").exists());
        let railpack = fs::read_to_string(root.join("private/railpack.json")).unwrap();
        let private: serde_json::Value = serde_json::from_str(&railpack).unwrap();
        assert!(
            private[0]["variables"]
                .get("RAILPACK_CONFIG_FILE")
                .is_none()
        );
        assert_eq!(
            private[0]["sealed"]["RAILPACK_CONFIG_FILE"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert!(!railpack.contains("marker-sealed-token"));
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "fixed capture fixture")]
    fn an_explicit_build_command_wins_over_a_sealed_one() {
        let root = tempfile::tempdir().unwrap();
        let (mut intent, _) = sealed_intent(7);
        let setting = EnvValue::Sealed(ployz_core::SealedValue {
            sealed: ployz_core::Ciphertext::from_bytes(vec![9; 48]),
            scope: ployz_core::SealedScope::Shared,
            key: "RAILPACK_BUILD_CMD".into(),
        });
        intent
            .target
            .first_mut()
            .unwrap()
            .container
            .environment
            .insert("RAILPACK_BUILD_CMD".into(), setting);
        let railpack = || BuildSpec {
            context: root.path().to_owned(),
            recipe: Recipe::Railpack {
                command: Some("make release".into()),
            },
        };
        let opened = OpenedSealed::new(
            BTreeMap::from([
                ("NPM_TOKEN".into(), "marker-sealed-token".into()),
                ("RAILPACK_BUILD_CMD".into(), "marker-sealed-command".into()),
            ]),
            Vec::new(),
        );
        let name = intent.target.first().unwrap().name.clone();
        for opened in [Some(opened), None] {
            let mut build = crate::build::capture(
                &intent,
                BTreeMap::from([(name.clone(), railpack())]),
                opened,
            )
            .unwrap();
            let captured = build.targets.pop().unwrap();
            let railpack =
                fs::read_to_string(captured.inputs.root().join("private/railpack.json")).unwrap();
            let private: serde_json::Value = serde_json::from_str(&railpack).unwrap();
            assert_eq!(
                private[0]["variables"]["RAILPACK_BUILD_CMD"],
                "make release"
            );
            assert!(private[0]["sealed"].get("RAILPACK_BUILD_CMD").is_none());
            assert!(private[0]["sealed"].get("NPM_TOKEN").is_some());
            assert!(!railpack.contains("marker-sealed"));
        }
    }

    #[test]
    fn a_sealed_value_without_an_identity_cannot_be_captured() {
        let root = tempfile::tempdir().unwrap();
        let (mut intent, _) = sealed_intent(7);
        intent.target.first_mut().unwrap().container.sealed_identity = None;
        let error = capture_one(&intent, dockerfile_spec(root.path(), MOUNTING_DOCKERFILE))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("Environment"), "{error}");
    }
}
