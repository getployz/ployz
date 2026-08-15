use std::{collections::BTreeSet, net::SocketAddr, time::Duration};

use ployz_core::{
    CADDY_VERIFY_PATH, ContainerKind, CreateDomainRecordsRequest, DnsRecordRequest, DnsRecordType,
    GetDomainRequest, InspectRequest, Machine, MachineId, MachineSelector, ReleaseDomainRequest,
    ReserveDomainRequest, RpcErrorCode, op,
};
use reqwest::{Client as HttpClient, redirect::Policy};
use thiserror::Error;

use crate::{
    caddy::SERVICE_NAME,
    connect::{Client, ConnectError},
};

const REACHABILITY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error("inspect Caddy Machine {machine_id}: {source}")]
    Inspect {
        machine_id: MachineId,
        #[source]
        source: ConnectError,
    },
    #[error("build hosted-DNS reachability client: {0}")]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    NoReachableMachines(#[from] NoReachableMachines),
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("no publicly reachable Caddy Machines found")]
pub struct NoReachableMachines;

impl Client {
    pub async fn reserve_domain(&mut self, endpoint: String) -> Result<String, ConnectError> {
        self.call::<op::ReserveDomain>(ReserveDomainRequest { endpoint }, None)
            .await
            .map(|domain| domain.name)
    }

    pub async fn domain(&mut self) -> Result<String, ConnectError> {
        self.call::<op::GetDomain>(GetDomainRequest {}, None)
            .await
            .map(|domain| domain.name)
    }

    pub async fn release_domain(&mut self) -> Result<String, ConnectError> {
        self.call::<op::ReleaseDomain>(ReleaseDomainRequest {}, None)
            .await
            .map(|domain| domain.name)
    }

    async fn create_domain_records(
        &mut self,
        records: Vec<DnsRecordRequest>,
    ) -> Result<(), ConnectError> {
        self.call::<op::CreateDomainRecords>(CreateDomainRecordsRequest { records }, None)
            .await
            .map(drop)
    }
}

pub async fn update_records_if_reserved(client: &mut Client) -> Result<(), Error> {
    match client.domain().await {
        Ok(_) => update_records_for_caddy(client).await,
        Err(ConnectError::Remote(error)) if error.code == RpcErrorCode::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub async fn update_records_for_caddy(client: &mut Client) -> Result<(), Error> {
    let live = client.live_services().await?;
    let caddy_machines = live
        .containers
        .successes
        .iter()
        .flat_map(|success| &success.value)
        .filter(|container| {
            container.kind == ContainerKind::ServiceContainer
                && container.service_name.as_str() == SERVICE_NAME
        })
        .map(|container| container.machine_id.clone())
        .collect::<BTreeSet<_>>();
    if caddy_machines.is_empty() {
        return Ok(());
    }

    let mut machines = Vec::new();
    for machine_id in caddy_machines {
        let target = MachineSelector::from(&machine_id);
        let details = client
            .call::<op::Inspect>(InspectRequest::default(), Some(&target))
            .await
            .map_err(|source| Error::Inspect {
                machine_id: machine_id.clone(),
                source,
            })?;
        let machine = details.machine.ok_or_else(|| Error::Inspect {
            machine_id,
            source: ConnectError::Attempt("inspect response omitted Machine details".into()),
        })?;
        if machine.public_ip.is_some() {
            machines.push(machine);
        }
    }

    let http = HttpClient::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(REACHABILITY_TIMEOUT)
        .build()
        .map_err(Error::from)?;
    let reachable = futures_util::future::join_all(machines.into_iter().map(|machine| {
        let http = &http;
        async move { probe_machine(http, &machine).await.then_some(machine) }
    }))
    .await
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let records = records_from_machines(&reachable)?;
    client
        .create_domain_records(records)
        .await
        .map_err(Into::into)
}

async fn probe_machine(http: &HttpClient, machine: &Machine) -> bool {
    let Some(public_ip) = machine.public_ip else {
        return false;
    };
    let address = SocketAddr::new(public_ip, 80);
    let Ok(response) = http
        .get(format!("http://{address}{CADDY_VERIFY_PATH}"))
        .send()
        .await
    else {
        return false;
    };
    let status = response.status().as_u16();
    let body = response.bytes().await.ok();
    reachability_matches(&machine.id, status, body.as_deref())
}

fn reachability_matches(machine_id: &MachineId, status: u16, body: Option<&[u8]>) -> bool {
    status == 200 && body == Some(machine_id.as_str().as_bytes())
}

fn records_from_machines(
    machines: &[Machine],
) -> Result<Vec<DnsRecordRequest>, NoReachableMachines> {
    if machines.is_empty() {
        return Err(NoReachableMachines);
    }
    let mut ipv4 = BTreeSet::new();
    let mut ipv6 = BTreeSet::new();
    for address in machines.iter().filter_map(|machine| machine.public_ip) {
        match address {
            std::net::IpAddr::V4(address) => {
                ipv4.insert(address.to_string());
            }
            std::net::IpAddr::V6(address) => {
                ipv6.insert(address.to_string());
            }
        }
    }
    let mut records = Vec::new();
    if !ipv4.is_empty() {
        records.push(DnsRecordRequest {
            name: "*".into(),
            record_type: DnsRecordType::A,
            values: ipv4.into_iter().collect(),
        });
    }
    if !ipv6.is_empty() {
        records.push(DnsRecordRequest {
            name: "*".into(),
            record_type: DnsRecordType::Aaaa,
            values: ipv6.into_iter().collect(),
        });
    }
    if records.is_empty() {
        Err(NoReachableMachines)
    } else {
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use ployz_core::{DnsRecordRequest, DnsRecordType, Machine, MachineId};

    use super::{NoReachableMachines, reachability_matches, records_from_machines};

    #[test]
    fn wildcard_records_include_each_present_address_family_once() {
        let ipv4 = machine('1', "192.0.2.1");
        let ipv6 = machine('2', "2001:db8::1");

        assert_eq!(
            records_from_machines(std::slice::from_ref(&ipv4)).unwrap(),
            vec![DnsRecordRequest {
                name: "*".into(),
                record_type: DnsRecordType::A,
                values: vec!["192.0.2.1".into()],
            }]
        );
        assert_eq!(
            records_from_machines(std::slice::from_ref(&ipv6)).unwrap(),
            vec![DnsRecordRequest {
                name: "*".into(),
                record_type: DnsRecordType::Aaaa,
                values: vec!["2001:db8::1".into()],
            }]
        );
        assert_eq!(
            records_from_machines(&[ipv4, ipv6]).unwrap(),
            vec![
                DnsRecordRequest {
                    name: "*".into(),
                    record_type: DnsRecordType::A,
                    values: vec!["192.0.2.1".into()],
                },
                DnsRecordRequest {
                    name: "*".into(),
                    record_type: DnsRecordType::Aaaa,
                    values: vec!["2001:db8::1".into()],
                },
            ]
        );
        assert_eq!(records_from_machines(&[]), Err(NoReachableMachines));
    }

    #[test]
    fn reachability_requires_status_200_and_the_exact_machine_id_bytes() {
        let machine_id = MachineId::parse("1".repeat(32)).unwrap();
        let exact = machine_id.as_str().as_bytes();

        assert!(reachability_matches(&machine_id, 200, Some(exact)));
        assert!(!reachability_matches(&machine_id, 204, Some(exact)));
        assert!(!reachability_matches(&machine_id, 200, Some(b"")));
        assert!(!reachability_matches(&machine_id, 200, Some(b"wrong")));
        let mut newline = exact.to_vec();
        newline.push(b'\n');
        assert!(!reachability_matches(&machine_id, 200, Some(&newline)));
        assert!(!reachability_matches(&machine_id, 200, None));
    }

    fn machine(seed: char, public_ip: &str) -> Machine {
        let mut machine: Machine = serde_json::from_value(serde_json::json!({
            "id": seed.to_string().repeat(32),
            "name": format!("machine-{seed}"),
            "subnet": format!("10.210.{}.0/24", seed.to_digit(10).unwrap()),
            "management_address": format!("fdcc::{seed}"),
            "public_key": vec![seed as u8; 32],
        }))
        .unwrap();
        machine.public_ip = Some(public_ip.parse::<IpAddr>().unwrap());
        machine
    }
}
