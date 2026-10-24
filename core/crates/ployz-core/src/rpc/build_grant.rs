//! Typed requests and outcomes for minting and inspecting a Build Grant.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Mint a Build Grant allowing one image push into this Machine's image ingest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MintBuildGrantRequest {
    /// The only repository the push may write, as Docker names it (`ployz-build/web`).
    pub repository: crate::BuildGrantRepository,
    /// The pusher's own public key, which becomes the grant's id. Without one the
    /// Machine makes the key and returns its secret. Only a daemon advertising
    /// [`crate::BUILD_GRANT_HOLDER_CAPABILITY`] reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub holder: Option<crate::BuildGrantId>,
}

/// A minted grant. The Machine keeps it in memory only; a restart ends it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BuildGrantMinted {
    /// Handle for [`InspectBuildGrantRequest`]; not secret.
    pub id: crate::BuildGrantId,
    #[serde(flatten)]
    pub key: MintedKey,
    /// The grant ends by itself this long after minting.
    pub expires_in_seconds: u64,
}

/// What the pusher dials a minted grant with.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum MintedKey {
    /// The Machine made the key: the grant's secret, for the pusher only.
    Grant {
        #[ts(type = "string")]
        grant: crate::BuildGrant,
    },
    /// The holder made the key and keeps it: only the Machine to dial.
    Holder {
        #[ts(type = "string")]
        #[serde(with = "machine_hex")]
        machine: crate::ManagementIdentity,
    },
}

mod machine_hex {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    use crate::ManagementIdentity;

    pub(super) fn serialize<S: Serializer>(
        machine: &ManagementIdentity,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&machine.to_hex())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ManagementIdentity, D::Error> {
        ManagementIdentity::parse_hex(&String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Read what a Build Grant received. Reading neither spends the grant nor shows its
/// secret; the grant ends by expiring. An unknown id, expired or lost to a daemon
/// restart, is `not_found`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct InspectBuildGrantRequest {
    pub id: crate::BuildGrantId,
}

/// What the Machine has received under a live grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BuildGrantInspected {
    /// `sha256:` digest of the manifest the Machine verified and stored, once the
    /// push completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub pushed: Option<crate::ImageDigest>,
    /// The grant ends by itself this long from now.
    pub expires_in_seconds: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_grant_reads_as_a_secret_or_a_machine_to_dial() {
        let machine = crate::ManagementIdentity::from_bytes([1; 32]);
        let grant = crate::BuildGrant::new(machine, [2; 32]);
        let id = crate::BuildGrantId::parse("d".repeat(64)).unwrap();
        let legacy = serde_json::json!({
            "id": id, "grant": grant.to_secret_string(), "expires_in_seconds": 10,
        });
        assert_eq!(
            serde_json::from_value::<BuildGrantMinted>(legacy.clone()).unwrap(),
            BuildGrantMinted {
                id,
                key: MintedKey::Grant { grant },
                expires_in_seconds: 10,
            }
        );
        let held = BuildGrantMinted {
            id,
            key: MintedKey::Holder { machine },
            expires_in_seconds: 10,
        };
        let wire = serde_json::to_value(&held).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({"id": id, "machine": "01".repeat(32), "expires_in_seconds": 10})
        );
        assert_eq!(
            serde_json::from_value::<BuildGrantMinted>(wire).unwrap(),
            held
        );
    }

    #[test]
    fn a_mint_request_without_a_holder_reads_as_before() {
        let repository = crate::BuildGrantRepository::parse("ployz-build/web").unwrap();
        let request: MintBuildGrantRequest =
            serde_json::from_value(serde_json::json!({"repository": "ployz-build/web"})).unwrap();
        assert_eq!(
            request,
            MintBuildGrantRequest {
                repository,
                holder: None
            }
        );
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({"repository": "ployz-build/web"})
        );
    }
}
