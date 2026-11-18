use ployz_core::{MachineSubnet, Region};

fn subnet(text: &str) -> MachineSubnet {
    MachineSubnet::parse(text).unwrap()
}

fn first(servers: &[(&str, Option<&str>)]) -> Option<String> {
    Region::of_first_server(
        servers
            .iter()
            .map(|(subnet_, label)| (subnet(subnet_), *label)),
    )
    .map(String::from)
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
