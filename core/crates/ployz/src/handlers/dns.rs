use clap::ArgMatches;
use ployz_core::{GetDomainRequest, ReleaseDomainRequest, ReserveDomainRequest, op};

use super::{Error, connect_client, leaf_matches, required, runtime};

pub(super) fn reserve(root: &ArgMatches) -> Result<(), Error> {
    let endpoint = required(leaf_matches(root), "endpoint")?;
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let domain = client
            .call::<op::ReserveDomain>(ReserveDomainRequest { endpoint }, None)
            .await?;
        println!("Reserved Cluster domain: {}", domain.name);
        crate::dns::update_records_for_ingress(&mut client).await?;
        Ok(())
    })
}

pub(super) fn show(root: &ArgMatches) -> Result<(), Error> {
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let domain = client
            .call::<op::GetDomain>(GetDomainRequest {}, None)
            .await?;
        println!("{}", domain.name);
        Ok(())
    })
}

pub(super) fn release(root: &ArgMatches) -> Result<(), Error> {
    runtime()?.block_on(async {
        let mut client = connect_client(root, None).await?;
        let domain = client
            .call::<op::ReleaseDomain>(ReleaseDomainRequest {}, None)
            .await?;
        println!("Released Cluster domain: {}", domain.name);
        Ok(())
    })
}
