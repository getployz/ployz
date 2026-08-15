use clap::ArgMatches;

use super::{Error, connect_client, leaf_matches, required, runtime};

pub(super) fn reserve(root: &ArgMatches) -> Result<(), Error> {
    let endpoint = required(leaf_matches(root), "endpoint")?;
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let domain = client.reserve_domain(endpoint).await?;
        println!("Reserved Cluster domain: {domain}");
        crate::dns::update_records_for_caddy(&mut client).await?;
        Ok(())
    })
}

pub(super) fn show(root: &ArgMatches) -> Result<(), Error> {
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let domain = client.domain().await?;
        println!("{domain}");
        Ok(())
    })
}

pub(super) fn release(root: &ArgMatches) -> Result<(), Error> {
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let domain = client.release_domain().await?;
        println!("Released Cluster domain: {domain}");
        Ok(())
    })
}
