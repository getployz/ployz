use tokio_postgres::Client;

use super::write;
use crate::config::RegionConfig;
use crate::control::test_pg::{connect, postgres};
use crate::domain::{Region, RegionName};

fn region(code: &str, name: Option<&str>) -> RegionConfig {
    RegionConfig {
        region: Region::parse(code).unwrap(),
        name: name.map(|n| RegionName::parse(n).unwrap()),
    }
}

async fn stored(admin: &Client) -> Vec<(String, Option<String>)> {
    admin
        .query(
            "SELECT region, name FROM uptime_region ORDER BY region",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

fn rows(pairs: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    pairs
        .iter()
        .map(|(code, name)| ((*code).to_owned(), name.map(str::to_owned)))
        .collect()
}

#[tokio::test]
async fn boot_names_regions_renames_them_and_keeps_retired_ones() {
    let (pg, admin) = postgres().await;
    let url = pg
        .role(
            &admin,
            "uptime_control",
            include_str!("../../control-role.sql"),
        )
        .await;
    let control = connect(&url).await;

    write(
        &control,
        &[region("ams", Some("Amsterdam")), region("iad", None)],
    )
    .await
    .unwrap();
    assert_eq!(
        stored(&admin).await,
        rows(&[("ams", Some("Amsterdam")), ("iad", None)])
    );

    write(
        &control,
        &[
            region("iad", Some("Virginia")),
            region("sin", Some("Singapore")),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        stored(&admin).await,
        rows(&[
            ("ams", Some("Amsterdam")),
            ("iad", Some("Virginia")),
            ("sin", Some("Singapore")),
        ]),
        "iad renamed, sin added, retired ams kept"
    );

    write(&control, &[region("sin", None)]).await.unwrap();
    assert_eq!(
        stored(&admin).await,
        rows(&[
            ("ams", Some("Amsterdam")),
            ("iad", Some("Virginia")),
            ("sin", None),
        ]),
        "a listed bare code clears its name"
    );
}
