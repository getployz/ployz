use ployz_core::authority::Timestamp;
use ployz_core::{JoinedServer, MachineSubnet, Region};

fn subnet(text: &str) -> MachineSubnet {
    MachineSubnet::parse(text).unwrap()
}

fn first(servers: &[(&str, Option<&str>)]) -> Option<String> {
    first_joined(
        &servers
            .iter()
            .map(|(subnet, label)| (*subnet, None, *label))
            .collect::<Vec<_>>(),
    )
}

fn first_joined(servers: &[(&str, Option<u64>, Option<&str>)]) -> Option<String> {
    Region::of_first_server(servers.iter().map(|server| read(*server))).map(String::from)
}

fn read<'a>((subnet_, joined, label): (&str, Option<u64>, Option<&'a str>)) -> JoinedServer<'a> {
    JoinedServer::Read {
        subnet: subnet(subnet_),
        joined: joined.map(|millis| Timestamp::from_millis(millis).unwrap()),
        label,
    }
}

#[test]
fn a_region_is_up_to_sixty_three_characters() {
    assert!(Region::parse("a".repeat(63)).is_ok());
    assert!(Region::parse("a".repeat(64)).is_err());
    assert!(Region::parse("").is_err());
    assert_eq!(Region::parse("EU_west.1").unwrap().as_str(), "eu_west.1");
    assert!(Region::parse("eu/west").is_err());
}

#[test]
fn a_new_environment_starts_in_the_first_servers_region() {
    assert_eq!(
        first(&[
            ("10.210.1.0/24", Some("syd")),
            ("10.210.0.0/24", Some("Default")),
        ]),
        Some("default".to_owned())
    );
}

#[test]
fn the_first_server_is_the_lowest_subnet_not_the_first_written() {
    assert_eq!(
        first(&[
            ("10.210.10.0/24", Some("later")),
            ("10.210.9.0/24", Some("earlier")),
        ]),
        Some("earlier".to_owned())
    );
}

#[test]
fn a_cluster_whose_first_server_predates_regions_starts_environments_in_any_region() {
    assert_eq!(
        first(&[
            ("10.210.0.0/24", None),
            ("10.210.1.0/24", None),
            ("10.210.2.0/24", Some("default")),
        ]),
        None
    );
}

#[test]
fn a_first_server_with_a_label_that_is_no_region_starts_environments_in_any_region() {
    assert_eq!(
        first(&[
            ("10.210.0.0/24", Some("eu/west")),
            ("10.210.1.0/24", Some("default"))
        ]),
        None
    );
    assert_eq!(first(&[]), None);
}

#[test]
fn a_cluster_with_a_server_that_went_unread_starts_environments_in_any_region() {
    let default = read(("10.210.0.0/24", Some(1_000), Some("default")));
    assert_eq!(
        Region::of_first_server([default]).map(String::from),
        Some("default".to_owned())
    );
    assert_eq!(
        Region::of_first_server([default, JoinedServer::Unread]),
        None,
        "the unread Server may have joined first, in another region or none"
    );
}

#[test]
fn a_mixed_cluster_starts_environments_in_any_region_whichever_subnet_is_lowest() {
    assert_eq!(
        first(&[("10.210.0.0/24", Some("default")), ("10.210.1.0/24", None),]),
        None,
        "the Server on the freed lowest subnet joined last; the unlabelled one is older"
    );
}

#[test]
fn a_reused_subnet_does_not_make_the_newest_server_first() {
    assert_eq!(
        first_joined(&[
            ("10.210.0.0/24", Some(2_000), Some("default")),
            ("10.210.1.0/24", Some(1_000), Some("syd")),
        ]),
        Some("syd".to_owned()),
        "the Server on the freed lowest subnet was approved after the one on the next"
    );
}

#[test]
fn a_server_with_no_recorded_join_comes_before_those_that_have_one() {
    assert_eq!(
        first_joined(&[
            ("10.210.0.0/24", Some(2_000), Some("later")),
            ("10.210.5.0/24", None, Some("founder")),
        ]),
        Some("founder".to_owned())
    );
}

#[test]
fn servers_joined_at_the_same_time_are_told_apart_by_subnet() {
    assert_eq!(
        first_joined(&[
            ("10.210.4.0/24", Some(1_000), Some("b")),
            ("10.210.3.0/24", Some(1_000), Some("a")),
        ]),
        Some("a".to_owned())
    );
}
