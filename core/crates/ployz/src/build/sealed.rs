//! Sealed values a Build reads: kept sealed in private material, shaped for its
//! recipe, and refused where a recipe would record one.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use ployz_core::{DockerfileFacts, EnvValue, RequestedServiceSpec};
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
    /// The names, for the Build's record.
    pub(super) names: Vec<String>,
}

/// Keep the Sealed values `service` reads in private upload material and shape
/// them for `recipe`.
///
/// # Errors
/// Fails without the Deploy's Environment to seal for, when the private file
/// cannot be written, or when `recipe` would record a value.
pub(super) fn capture(
    service: &RequestedServiceSpec,
    recipe: &Recipe,
    inputs: &BuildInputs,
) -> Result<Sealed, Error> {
    let values: BTreeMap<String, EnvValue> = service
        .container
        .environment
        .iter()
        .filter(|(_, value)| value.text().is_none())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut sealed = Sealed::default();
    if values.is_empty() {
        return Ok(sealed);
    }
    inputs.sealed(&SealedCapture {
        identity: service
            .container
            .sealed_identity
            .as_ref()
            .ok_or_else(|| invalid("a sealed value needs the Deploy's Environment"))?,
        values: &values,
    })?;
    match recipe {
        Recipe::Railpack { .. } => {
            refuse_railpack_settings(values.keys())?;
            sealed.fingerprints = values
                .iter()
                .map(|(name, value)| (name.clone(), fingerprint(value)))
                .collect();
        }
        Recipe::Dockerfile(dockerfile) => {
            refuse_declared_args(dockerfile, values.keys())?;
            sealed
                .args
                .insert(SEALED_HASH_ARG.into(), sealed_hash(&values));
            sealed.secrets = values
                .keys()
                .map(|name| {
                    let file = ployz_build::sealed_file(name);
                    (name.clone(), CapturedSecret { file })
                })
                .collect();
        }
    }
    sealed.names = values.into_keys().collect();
    Ok(sealed)
}

/// A Sealed value a Dockerfile declares as `ARG` would reach BuildKit as a
/// build arg, which records it in the image. Only a secret mount may read it.
fn refuse_declared_args<'name>(
    dockerfile: &Path,
    mut sealed: impl Iterator<Item = &'name String>,
) -> Result<(), Error> {
    let contents = std::fs::read_to_string(dockerfile)
        .map_err(|error| Error::Io(format!("read Dockerfile: {error}")))?;
    let declared = DockerfileFacts::parse(&contents).args;
    match sealed.find(|name| declared.contains(*name)) {
        Some(name) => Err(Error::Sealed(format!(
            "{name} is sealed. Read it with RUN --mount=type=secret,id={name},env={name}."
        ))),
        None => Ok(()),
    }
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

/// `private/sealed.json`: the Sealed values a Build reads, still sealed, in
/// the shape the Builder's env opening already accepts.
#[derive(Serialize)]
pub(super) struct SealedCapture<'value> {
    identity: &'value ployz_core::SealedIdentity,
    values: &'value BTreeMap<String, EnvValue>,
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
    }

    #[test]
    fn a_dockerfile_declaring_a_sealed_value_as_arg_is_refused_by_name() {
        let root = tempfile::tempdir().unwrap();
        let (intent, sealed) = sealed_intent(7);
        let ciphertext = serde_json::to_string(&sealed).unwrap();
        for dockerfile in [
            "FROM scratch\nARG NPM_TOKEN\n",
            "FROM scratch\nARG NPM_TOKEN=default\n",
            "FROM scratch AS base\narg PLAIN NPM_TOKEN\nFROM scratch\n",
        ] {
            let error = capture_one(&intent, dockerfile_spec(root.path(), dockerfile))
                .err()
                .unwrap()
                .to_string();
            assert_eq!(
                error,
                "NPM_TOKEN is sealed. Read it with RUN --mount=type=secret,id=NPM_TOKEN,env=NPM_TOKEN.",
                "{dockerfile}"
            );
            assert!(!error.contains(&ciphertext));
            assert!(!error.contains("visible"));
        }
        for dockerfile in [MOUNTING_DOCKERFILE, "FROM scratch\nARG PLAIN\n"] {
            capture_one(&intent, dockerfile_spec(root.path(), dockerfile)).unwrap();
        }
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
