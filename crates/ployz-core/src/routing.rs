use thiserror::Error;
use tonic::metadata::{MetadataMap, MetadataValue};

use crate::{FanoutSelector, MachineTarget};

pub const ONE_TARGET_HEADER: &str = "machine";
pub const ONE_TARGET_BINARY_HEADER: &str = "machine-bin";
pub const MANY_TARGETS_HEADER: &str = "machines";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoutingRequest {
    Local,
    One(MachineTarget),
    Many(Vec<FanoutSelector>),
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RoutingMetadataError {
    #[error("both one-target and fan-out routing metadata are set")]
    ConflictingTargets,
    #[error("both text and binary one-target routing metadata are set")]
    ConflictingSingleTargets,
    #[error("one-target routing metadata must contain exactly one Machine selector")]
    InvalidSingleTarget,
    #[error("routing metadata contains an invalid Machine selector")]
    InvalidTarget,
}

pub fn apply_one_target(metadata: &mut MetadataMap, target: &MachineTarget) {
    if is_ascii_graphic(target.as_str()) {
        metadata.insert(
            ONE_TARGET_HEADER,
            target.as_str().parse().expect("visible ASCII metadata"),
        );
    } else {
        metadata.insert_bin(
            ONE_TARGET_BINARY_HEADER,
            MetadataValue::from_bytes(target.as_str().as_bytes()),
        );
    }
}

pub fn apply_many_targets(
    metadata: &mut MetadataMap,
    targets: &[FanoutSelector],
) -> Result<(), RoutingMetadataError> {
    if targets
        .iter()
        .any(|target| !is_ascii_graphic(target.as_str()))
    {
        return Err(RoutingMetadataError::InvalidTarget);
    }
    for target in targets {
        metadata.append(
            MANY_TARGETS_HEADER,
            target.as_str().parse().expect("visible ASCII metadata"),
        );
    }
    Ok(())
}

pub fn clear_routing_headers(headers: &mut tonic::codegen::http::HeaderMap) {
    headers.remove(ONE_TARGET_HEADER);
    headers.remove(ONE_TARGET_BINARY_HEADER);
    headers.remove(MANY_TARGETS_HEADER);
}

fn is_ascii_graphic(value: &str) -> bool {
    value.bytes().all(|byte| byte.is_ascii_graphic())
}

pub fn routing_from_metadata(
    metadata: &MetadataMap,
) -> Result<RoutingRequest, RoutingMetadataError> {
    let has_text_target = metadata.get(ONE_TARGET_HEADER).is_some();
    let has_binary_target = metadata.get_bin(ONE_TARGET_BINARY_HEADER).is_some();
    if has_text_target && has_binary_target {
        return Err(RoutingMetadataError::ConflictingSingleTargets);
    }
    let has_one = has_text_target || has_binary_target;
    let has_many = metadata.get(MANY_TARGETS_HEADER).is_some();
    if has_one && has_many {
        return Err(RoutingMetadataError::ConflictingTargets);
    }
    if has_binary_target {
        let encoded = metadata
            .get_bin(ONE_TARGET_BINARY_HEADER)
            .ok_or(RoutingMetadataError::InvalidTarget)?
            .to_bytes()
            .map_err(|_| RoutingMetadataError::InvalidTarget)?;
        let value =
            std::str::from_utf8(&encoded).map_err(|_| RoutingMetadataError::InvalidTarget)?;
        return MachineTarget::parse(value)
            .map(RoutingRequest::One)
            .map_err(|_| RoutingMetadataError::InvalidTarget);
    }
    if has_text_target {
        let mut selectors = ascii_parsed(metadata, ONE_TARGET_HEADER)?;
        if selectors.len() != 1 {
            return Err(RoutingMetadataError::InvalidSingleTarget);
        }
        return Ok(RoutingRequest::One(selectors.remove(0)));
    }
    if has_many {
        Ok(RoutingRequest::Many(ascii_parsed(
            metadata,
            MANY_TARGETS_HEADER,
        )?))
    } else {
        Ok(RoutingRequest::Local)
    }
}

fn ascii_parsed<T: std::str::FromStr>(
    metadata: &MetadataMap,
    name: &'static str,
) -> Result<Vec<T>, RoutingMetadataError> {
    metadata
        .get_all(name)
        .iter()
        .map(|value| {
            value
                .to_str()
                .map_err(|_| RoutingMetadataError::InvalidTarget)
                .and_then(|value| {
                    value
                        .parse()
                        .map_err(|_| RoutingMetadataError::InvalidTarget)
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_selectors_use_text_metadata() {
        let selector = MachineTarget::parse("edge-1").unwrap();
        let mut metadata = MetadataMap::new();
        apply_one_target(&mut metadata, &selector);

        assert!(metadata.get_bin(ONE_TARGET_BINARY_HEADER).is_none());
        assert_eq!(
            routing_from_metadata(&metadata).unwrap(),
            RoutingRequest::One(selector)
        );
    }

    #[test]
    fn unicode_selectors_round_trip_through_binary_metadata() {
        let selector = MachineTarget::parse("München edge").unwrap();
        let mut metadata = MetadataMap::new();
        apply_one_target(&mut metadata, &selector);

        assert!(metadata.get(ONE_TARGET_HEADER).is_none());
        let headers = metadata.clone().into_headers();
        assert_eq!(
            routing_from_metadata(&MetadataMap::from_headers(headers)).unwrap(),
            RoutingRequest::One(selector)
        );
    }

    #[test]
    fn text_and_binary_one_target_headers_conflict() {
        let mut metadata = MetadataMap::new();
        apply_one_target(&mut metadata, &MachineTarget::parse("edge-1").unwrap());
        metadata.insert_bin(
            ONE_TARGET_BINARY_HEADER,
            MetadataValue::from_bytes(b"other"),
        );

        assert_eq!(
            routing_from_metadata(&metadata).unwrap_err(),
            RoutingMetadataError::ConflictingSingleTargets
        );
    }

    #[test]
    fn one_target_and_fan_out_headers_conflict() {
        let mut metadata = MetadataMap::new();
        apply_one_target(&mut metadata, &MachineTarget::parse("edge-1").unwrap());
        metadata.insert(
            MANY_TARGETS_HEADER,
            "edge-2".parse().expect("visible ASCII metadata"),
        );

        assert_eq!(
            routing_from_metadata(&metadata).unwrap_err(),
            RoutingMetadataError::ConflictingTargets
        );
    }

    #[test]
    fn many_targets_preserve_order() {
        let targets = vec![
            FanoutSelector::parse("edge-1").unwrap(),
            FanoutSelector::All,
        ];
        let mut metadata = MetadataMap::new();
        apply_many_targets(&mut metadata, &targets).unwrap();

        assert_eq!(
            routing_from_metadata(&metadata).unwrap(),
            RoutingRequest::Many(targets)
        );
    }

    #[test]
    fn one_target_rejects_star_and_keeps_all_as_identity() {
        let mut star = MetadataMap::new();
        star.insert(
            ONE_TARGET_HEADER,
            "*".parse().expect("visible ASCII metadata"),
        );
        assert_eq!(
            routing_from_metadata(&star).unwrap_err(),
            RoutingMetadataError::InvalidTarget
        );

        let mut named_all = MetadataMap::new();
        apply_one_target(&mut named_all, &MachineTarget::parse("all").unwrap());
        assert_eq!(
            routing_from_metadata(&named_all).unwrap(),
            RoutingRequest::One(MachineTarget::parse("all").unwrap())
        );
    }

    #[test]
    fn many_targets_accept_star_and_keep_all_as_identity() {
        let targets = vec![FanoutSelector::All, FanoutSelector::parse("all").unwrap()];
        let mut metadata = MetadataMap::new();
        apply_many_targets(&mut metadata, &targets).unwrap();

        assert_eq!(
            routing_from_metadata(&metadata).unwrap(),
            RoutingRequest::Many(targets)
        );
    }

    #[test]
    fn many_targets_reject_non_ascii() {
        let mut metadata = MetadataMap::new();

        assert_eq!(
            apply_many_targets(
                &mut metadata,
                &[FanoutSelector::parse("München edge").unwrap()]
            )
            .unwrap_err(),
            RoutingMetadataError::InvalidTarget
        );
        assert!(metadata.get(MANY_TARGETS_HEADER).is_none());
    }

    #[test]
    fn clear_routing_headers_drops_one_and_many_headers() {
        let mut metadata = MetadataMap::new();
        apply_one_target(
            &mut metadata,
            &MachineTarget::parse("München edge").unwrap(),
        );
        metadata.insert(
            ONE_TARGET_HEADER,
            "edge-1".parse().expect("visible ASCII metadata"),
        );
        apply_many_targets(&mut metadata, &[FanoutSelector::parse("edge-2").unwrap()]).unwrap();
        let mut headers = metadata.into_headers();

        clear_routing_headers(&mut headers);

        assert_eq!(
            routing_from_metadata(&MetadataMap::from_headers(headers)).unwrap(),
            RoutingRequest::Local
        );
    }

    #[test]
    fn missing_routing_metadata_is_local() {
        assert_eq!(
            routing_from_metadata(&MetadataMap::new()).unwrap(),
            RoutingRequest::Local
        );
    }

    #[test]
    fn duplicate_text_targets_are_invalid() {
        let mut metadata = MetadataMap::new();
        metadata.append(
            ONE_TARGET_HEADER,
            "edge-1".parse().expect("visible ASCII metadata"),
        );
        metadata.append(
            ONE_TARGET_HEADER,
            "edge-2".parse().expect("visible ASCII metadata"),
        );

        assert_eq!(
            routing_from_metadata(&metadata).unwrap_err(),
            RoutingMetadataError::InvalidSingleTarget
        );
    }
}
