use clap::ArgMatches;

use super::{Error, connect_client, leaf_matches, required, runtime};

pub(super) fn reserve(root: &ArgMatches) -> Result<(), Error> {
    let endpoint = required(leaf_matches(root), "endpoint")?;
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let domain = client
            .reserve_domain(endpoint)
            .await
            .map_err(|error| error.to_string())?;
        println!("Reserved Cluster domain: {domain}");
        crate::dns::update_records_for_caddy(&mut client)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    })
}

pub(super) fn show(root: &ArgMatches) -> Result<(), Error> {
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let domain = client.domain().await.map_err(|error| error.to_string())?;
        println!("{domain}");
        Ok(())
    })
}

pub(super) fn release(root: &ArgMatches) -> Result<(), Error> {
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let domain = client
            .release_domain()
            .await
            .map_err(|error| error.to_string())?;
        println!("Released Cluster domain: {domain}");
        Ok(())
    })
}
