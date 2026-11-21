//! A Volume's origin crosses daemons of different releases on disk, in labels and in
//! headers, so each encoding is pinned by `fixtures/volume_origin.json`, which the
//! verify-server harness reads too.

use ployz_core::{
    DockerVolumeName, MountOrigins, VOLUME_ORIGIN_CAPABILITY, VolumeBirth, VolumeOrigin,
    authority::canonical::to_canonical, claims_capability, single_origin,
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/volume_origin.json")).unwrap()
}

fn at<'a>(value: &'a Value, key: &str) -> &'a Value {
    value
        .get(key)
        .unwrap_or_else(|| panic!("the fixture has no {key}"))
}

fn cases<'a>(fixture: &'a Value, key: &str) -> &'a Vec<Value> {
    at(fixture, key).as_array().unwrap()
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect()
}

#[test]
fn a_born_origin_hashes_the_births_canonical_json() {
    let fixture = fixture();
    for case in cases(&fixture, "born") {
        let birth: VolumeBirth = serde_json::from_value(at(case, "birth").clone()).unwrap();
        assert_eq!(
            to_canonical(&birth).unwrap(),
            at(case, "canonical").as_str().unwrap()
        );
        let origin = VolumeOrigin::of(&birth).unwrap();
        assert_eq!(origin.to_string(), at(case, "origin").as_str().unwrap());
        let physical = DockerVolumeName::parse("files_previews").unwrap();
        assert_eq!(VolumeOrigin::expected(Some(&birth), &physical), Ok(origin));
    }
}

#[test]
fn a_legacy_origin_hashes_the_physical_name() {
    let fixture = fixture();
    for case in cases(&fixture, "legacy") {
        let physical = DockerVolumeName::parse(at(case, "physical").as_str().unwrap()).unwrap();
        let origin = at(case, "origin").as_str().unwrap();
        assert_eq!(VolumeOrigin::legacy(&physical).to_string(), origin);
        assert_eq!(
            VolumeOrigin::expected(None, &physical).unwrap().to_string(),
            origin
        );
    }
}

#[test]
fn malformed_births_and_origins_refuse() {
    let fixture = fixture();
    for case in cases(&fixture, "births_refused") {
        let mut birth = case.clone();
        let why = birth.as_object_mut().unwrap().remove("why").unwrap();
        assert!(
            serde_json::from_value::<VolumeBirth>(birth).is_err(),
            "{why}"
        );
    }
    for origin in strings(at(&fixture, "origins_refused")) {
        assert!(VolumeOrigin::parse(origin).is_err(), "{origin:?}");
    }
}

#[test]
fn a_containers_origins_label_is_canonical_json_by_physical_name() {
    let fixture = fixture();
    for case in cases(&fixture, "mount_origins") {
        let label = at(case, "label").as_str().unwrap();
        let origins: MountOrigins = serde_json::from_value(at(case, "origins").clone()).unwrap();
        assert_eq!(origins.label().unwrap(), label);
        assert_eq!(MountOrigins::parse(label).unwrap(), origins);
        for (name, origin) in at(case, "origins").as_object().unwrap() {
            let name = DockerVolumeName::parse(name).unwrap();
            assert_eq!(
                origins.get(&name).map(|origin| origin.to_string()),
                origin.as_str().map(str::to_owned)
            );
        }
        let unlisted = DockerVolumeName::parse("files_other").unwrap();
        assert_eq!(origins.get(&unlisted), None);
    }
    for label in strings(at(&fixture, "mount_origins_refused")) {
        assert!(MountOrigins::parse(label).is_err(), "{label:?}");
    }
}

#[test]
fn a_reply_carries_at_most_one_well_formed_origin() {
    let fixture = fixture();
    for case in cases(&fixture, "reply_header") {
        let values = strings(at(case, "values"));
        let read = single_origin(values.iter().copied());
        if case.get("refused").and_then(Value::as_bool) == Some(true) {
            assert!(read.is_err(), "{values:?}");
        } else {
            assert_eq!(
                read.unwrap().map(|origin| origin.to_string()),
                case.get("origin")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                "{values:?}"
            );
        }
    }
}

#[test]
fn a_pull_claims_the_capability_only_by_its_exact_name() {
    let fixture = fixture();
    for case in cases(&fixture, "capabilities") {
        let values = strings(at(case, "values"));
        assert_eq!(
            claims_capability(values.iter().copied(), VOLUME_ORIGIN_CAPABILITY),
            at(case, "claims").as_bool().unwrap(),
            "{values:?}"
        );
    }
}
