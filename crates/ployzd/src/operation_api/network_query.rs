//! Fresh, intent-driven projection of the internal DNS answer and gather coverage.

use std::collections::BTreeMap;

use crate::intent::service::NatsIntentReader;
use crate::roles::dns::records::{InternalServiceName, internal_dns_records};
use crate::roles::machine::client::{NatsMachineFactsReader, read_available_machine_facts_by_id};
use ployz_core::dataplane::INTERNAL_DNS_SUFFIX;
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::MachineFactsSnapshot;
use ployz_sdk_types::{
    NetworkResolveError, NetworkResolveMachineTestimony, NetworkResolveRequest,
    NetworkResolveResult,
};

#[derive(Clone)]
pub struct NetworkQueryService {
    intent_reader: NatsIntentReader,
    facts_reader: NatsMachineFactsReader,
}

impl NetworkQueryService {
    #[must_use]
    pub(crate) const fn new(
        intent_reader: NatsIntentReader,
        facts_reader: NatsMachineFactsReader,
    ) -> Self {
        Self {
            intent_reader,
            facts_reader,
        }
    }

    pub(crate) async fn resolve(
        &self,
        request: NetworkResolveRequest,
    ) -> Result<NetworkResolveResult, NetworkResolveError> {
        let Some(name) = normalize_internal_name(&request.name) else {
            return Err(NetworkResolveError::InvalidName { name: request.name });
        };
        let intent = self.intent_reader.intent().await.map_err(|error| {
            NetworkResolveError::Unavailable {
                message: error.to_string(),
            }
        })?;
        let machine_ids = intent
            .active_machines
            .iter()
            .map(|machine| machine.machine_id.clone())
            .collect::<Vec<_>>();
        let facts =
            read_available_machine_facts_by_id(&self.facts_reader, machine_ids.clone()).await;
        Ok(network_resolve_result(name, &machine_ids, &facts))
    }
}

fn normalize_internal_name(name: &str) -> Option<InternalServiceName> {
    let labels = name.split('.').collect::<Vec<_>>();
    let normalized = match labels.as_slice() {
        [service] => format!("{service}.default.{INTERNAL_DNS_SUFFIX}"),
        [service, namespace] => format!("{service}.{namespace}.{INTERNAL_DNS_SUFFIX}"),
        [service, namespace, suffix] if suffix.eq_ignore_ascii_case(INTERNAL_DNS_SUFFIX) => {
            format!("{service}.{namespace}.{INTERNAL_DNS_SUFFIX}")
        }
        [] | [_, _, _] | [_, _, _, ..] => return None,
    };
    InternalServiceName::parse(&normalized)
}

fn network_resolve_result(
    name: InternalServiceName,
    machine_ids: &[MachineId],
    facts: &BTreeMap<MachineId, MachineFactsSnapshot>,
) -> NetworkResolveResult {
    let snapshots = facts.values().cloned().collect::<Vec<_>>();
    let addresses = internal_dns_records(&snapshots)
        .remove(&name)
        .unwrap_or_default()
        .into_iter()
        .map(|address| address.to_string())
        .collect();
    let machines = machine_ids
        .iter()
        .map(|machine_id| {
            if facts.contains_key(machine_id) {
                NetworkResolveMachineTestimony::Answered {
                    machine_id: machine_id.clone(),
                }
            } else {
                NetworkResolveMachineTestimony::NoAnswer {
                    machine_id: machine_id.clone(),
                }
            }
        })
        .collect();
    NetworkResolveResult {
        name: name.as_str().to_owned(),
        addresses,
        machines,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{network_resolve_result, normalize_internal_name};
    use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
    use ployz_sdk_types::NetworkResolveMachineTestimony;
    use ployz_test_support::containers;
    use ployz_test_support::ids::machine_id;

    #[test]
    fn normalizes_supported_name_forms() {
        let cases = [
            ("db", "db.default.internal"),
            ("db.team-a", "db.team-a.internal"),
            ("DB.Team-A.Internal", "db.team-a.internal"),
        ];

        for (query, expected) in cases {
            assert_eq!(
                normalize_internal_name(query)
                    .expect("supported internal name")
                    .as_str(),
                expected
            );
        }
    }

    #[test]
    fn rejects_invalid_name() {
        assert!(normalize_internal_name("db.team-a.internal.extra").is_none());
    }

    #[test]
    fn projects_all_answers_and_keeps_silent_machines() {
        let web_a = containers::observation("machine_a", "ctr_web_a")
            .with(containers::identity("web").namespace("team-a"))
            .running_at("10.42.1.8".parse().expect("valid IPv4"))
            .build();
        let web_b = containers::observation("machine_b", "ctr_web_b")
            .with(containers::identity("web").namespace("team-a"))
            .running_at("10.42.2.9".parse().expect("valid IPv4"))
            .build();
        let machine_ids = vec![
            machine_id("machine_a"),
            machine_id("machine_b"),
            machine_id("machine_c"),
        ];
        let machine_a_facts = machine_facts("machine_a", [web_a]);
        let machine_b_facts = machine_facts("machine_b", [web_b]);
        let facts = BTreeMap::from([
            (machine_a_facts.machine_id().clone(), machine_a_facts),
            (machine_b_facts.machine_id().clone(), machine_b_facts),
        ]);

        let result = network_resolve_result(
            normalize_internal_name("web.team-a").expect("valid query"),
            &machine_ids,
            &facts,
        );

        assert_eq!(
            result.addresses,
            vec!["10.42.1.8".to_owned(), "10.42.2.9".to_owned()]
        );
        assert_eq!(
            result.machines,
            vec![
                NetworkResolveMachineTestimony::Answered {
                    machine_id: machine_id("machine_a"),
                },
                NetworkResolveMachineTestimony::Answered {
                    machine_id: machine_id("machine_b"),
                },
                NetworkResolveMachineTestimony::NoAnswer {
                    machine_id: machine_id("machine_c"),
                },
            ]
        );
    }

    fn machine_facts(
        machine_id_value: &str,
        containers: impl IntoIterator<Item = ployz_core::machine_runtime::ManagedContainerObservation>,
    ) -> MachineFactsSnapshot {
        let machine_id = machine_id(machine_id_value);
        MachineFactsSnapshot::try_new(
            machine_id.clone(),
            MachineContainerObservationSnapshot::try_new(machine_id, containers)
                .expect("valid container snapshot"),
            None,
            ployz_test_support::fixtures::test_disk_space(),
            1,
        )
        .expect("valid machine facts")
    }
}
