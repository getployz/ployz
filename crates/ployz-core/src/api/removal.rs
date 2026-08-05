//! Typed, synchronous removal contracts for named Corrosion rows.

use serde::{Deserialize, Serialize};

use crate::corrosion::{
    AcceptedRow, ClusterDocument, NamespaceDocument, PeerDocument, RouteBindingDocument,
    ServiceDocument, StoredRow, read_roster_rows, read_rows,
};
use crate::ids::{NamespaceRowId, PeerId, RouteBindingRowId, ServiceRowId};
use crate::operation::RouteHostname;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NamedRemovalOutcome {
    Removed,
    AlreadyAbsent,
}

macro_rules! removal_contract {
    (
        request: $request:ident, reply: $reply:ident, refusal: $refusal:ident,
        selection: $selection:ident, id: $id:ty, id_field: $id_field:ident,
        handle_field: $handle_field:ident, handle: $handle:ty, candidates: $candidates:ident
    ) => {
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        #[serde(deny_unknown_fields)]
        pub struct $request {
            pub $handle_field: $handle,
            pub $id_field: Option<$id>,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        pub struct $reply {
            pub $id_field: $id,
            pub outcome: NamedRemovalOutcome,
        }

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        #[serde(tag = "kind", rename_all = "snake_case")]
        pub enum $refusal {
            NotFound {
                $handle_field: $handle,
            },
            Ambiguous {
                $handle_field: $handle,
                $candidates: Vec<$id>,
            },
            NameMismatch {
                $id_field: $id,
                requested: $handle,
                found: $handle,
            },
            StoredRowUnselectable {
                $id_field: $id,
            },
            ConcurrentMutation {
                $id_field: $id,
            },
        }

        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum $selection {
            Delete {
                $id_field: $id,
                stored_document: String,
            },
            AlreadyAbsent {
                $id_field: $id,
            },
        }
    };
}

removal_contract! {
    request: PeerRemoveRequest, reply: PeerRemoveReply, refusal: PeerRemoveRefusal,
    selection: PeerRemoveSelection, id: PeerId, id_field: peer_id,
    handle_field: name, handle: String, candidates: peer_ids
}
removal_contract! {
    request: NamespaceRemoveRowRequest, reply: NamespaceRemoveRowReply,
    refusal: NamespaceRemoveRowRefusal, selection: NamespaceRemoveRowSelection,
    id: NamespaceRowId, id_field: namespace_id, handle_field: name, handle: String,
    candidates: namespace_ids
}
removal_contract! {
    request: RouteRemoveRequest, reply: RouteRemoveReply, refusal: RouteRemoveRefusal,
    selection: RouteRemoveSelection, id: RouteBindingRowId, id_field: route_id,
    handle_field: hostname, handle: RouteHostname, candidates: route_ids
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceRemoveRowRequest {
    pub namespace_id: NamespaceRowId,
    pub name: String,
    pub service_id: Option<ServiceRowId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ServiceRemoveRowReply {
    pub service_id: ServiceRowId,
    pub outcome: NamedRemovalOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceRemoveRowRefusal {
    NotFound {
        namespace_id: NamespaceRowId,
        name: String,
    },
    Ambiguous {
        namespace_id: NamespaceRowId,
        name: String,
        service_ids: Vec<ServiceRowId>,
    },
    IdentityMismatch {
        service_id: ServiceRowId,
        requested_namespace_id: NamespaceRowId,
        requested_name: String,
        found_namespace_id: NamespaceRowId,
        found_name: String,
    },
    StoredRowUnselectable {
        service_id: ServiceRowId,
    },
    ConcurrentMutation {
        service_id: ServiceRowId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceRemoveRowSelection {
    Delete {
        service_id: ServiceRowId,
        stored_document: String,
    },
    AlreadyAbsent {
        service_id: ServiceRowId,
    },
}

enum Selection<Id> {
    Delete { id: Id, stored_document: String },
    AlreadyAbsent { id: Id },
    NotFound,
    Ambiguous(Vec<Id>),
    NameMismatch { id: Id, found: String },
    StoredRowUnselectable { id: Id },
}

fn select_named<Document, Id, ParseId, IdString, Name>(
    raw_rows: &[StoredRow],
    accepted: Vec<AcceptedRow<Document>>,
    requested_name: &str,
    requested_id: Option<&Id>,
    parse_id: ParseId,
    id_string: IdString,
    name: Name,
) -> Selection<Id>
where
    Id: Clone + Ord,
    ParseId: Fn(&str) -> Id,
    IdString: Fn(&Id) -> &str,
    Name: Fn(&Document) -> String,
{
    if let Some(requested_id) = requested_id {
        let requested_key = accepted
            .iter()
            .map(|row| parse_id(&row.source.key))
            .find(|id| id == requested_id);
        if requested_key.is_none() {
            let stored = raw_rows
                .iter()
                .any(|row| row.key == id_string(requested_id));
            return if stored {
                Selection::StoredRowUnselectable {
                    id: requested_id.clone(),
                }
            } else {
                Selection::AlreadyAbsent {
                    id: requested_id.clone(),
                }
            };
        }
        let row = accepted
            .into_iter()
            .find(|row| parse_id(&row.source.key) == *requested_id)
            .expect("the accepted row was found above");
        let found = name(&row.value);
        if found != requested_name {
            return Selection::NameMismatch {
                id: requested_id.clone(),
                found,
            };
        }
        return Selection::Delete {
            id: requested_id.clone(),
            stored_document: row.source.document,
        };
    }

    let mut candidates = accepted
        .into_iter()
        .filter(|row| name(&row.value) == requested_name)
        .map(|row| (parse_id(&row.source.key), row.source.document))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    match candidates.as_slice() {
        [] => Selection::NotFound,
        [(id, stored_document)] => Selection::Delete {
            id: id.clone(),
            stored_document: stored_document.clone(),
        },
        [_, _, ..] => Selection::Ambiguous(candidates.into_iter().map(|(id, _)| id).collect()),
    }
}

pub fn select_peer_removal(
    cluster: &ClusterDocument,
    rows: Vec<StoredRow>,
    request: &PeerRemoveRequest,
) -> Result<PeerRemoveSelection, PeerRemoveRefusal> {
    let accepted = read_roster_rows::<PeerDocument>(cluster, rows.clone()).accepted;
    match select_named(
        &rows,
        accepted,
        &request.name,
        request.peer_id.as_ref(),
        |id| PeerId::try_new(id).expect("accepted row ids are canonical"),
        PeerId::as_str,
        |document| document.name.clone(),
    ) {
        Selection::Delete {
            id,
            stored_document,
        } => Ok(PeerRemoveSelection::Delete {
            peer_id: id,
            stored_document,
        }),
        Selection::AlreadyAbsent { id } => Ok(PeerRemoveSelection::AlreadyAbsent { peer_id: id }),
        Selection::NotFound => Err(PeerRemoveRefusal::NotFound {
            name: request.name.clone(),
        }),
        Selection::Ambiguous(peer_ids) => Err(PeerRemoveRefusal::Ambiguous {
            name: request.name.clone(),
            peer_ids,
        }),
        Selection::NameMismatch { id, found } => Err(PeerRemoveRefusal::NameMismatch {
            peer_id: id,
            requested: request.name.clone(),
            found,
        }),
        Selection::StoredRowUnselectable { id } => {
            Err(PeerRemoveRefusal::StoredRowUnselectable { peer_id: id })
        }
    }
}

macro_rules! ordinary_selector {
    (
        function: $function:ident, document: $document:ty, request: $request:ty,
        selection: $selection:ident, refusal: $refusal:ident, id: $id:ty,
        id_field: $id_field:ident, handle_field: $handle_field:ident,
        candidates: $candidates:ident, name: $name:expr
    ) => {
        pub fn $function(
            cluster_id: &crate::ids::ClusterId,
            rows: Vec<StoredRow>,
            request: &$request,
        ) -> Result<$selection, $refusal> {
            let accepted = read_rows::<$document>(cluster_id, rows.clone()).accepted;
            let requested_name = request.$handle_field.to_string();
            match select_named(
                &rows,
                accepted,
                &requested_name,
                request.$id_field.as_ref(),
                |id| <$id>::try_new(id).expect("accepted row ids are canonical"),
                <$id>::as_str,
                $name,
            ) {
                Selection::Delete {
                    id,
                    stored_document,
                } => Ok($selection::Delete {
                    $id_field: id,
                    stored_document,
                }),
                Selection::AlreadyAbsent { id } => Ok($selection::AlreadyAbsent { $id_field: id }),
                Selection::NotFound => Err($refusal::NotFound {
                    $handle_field: request.$handle_field.clone(),
                }),
                Selection::Ambiguous($candidates) => Err($refusal::Ambiguous {
                    $handle_field: request.$handle_field.clone(),
                    $candidates,
                }),
                Selection::NameMismatch { id, found } => Err($refusal::NameMismatch {
                    $id_field: id,
                    requested: request.$handle_field.clone(),
                    found: found
                        .try_into()
                        .expect("accepted document handle remains valid"),
                }),
                Selection::StoredRowUnselectable { id } => {
                    Err($refusal::StoredRowUnselectable { $id_field: id })
                }
            }
        }
    };
}

ordinary_selector! {
    function: select_namespace_removal, document: NamespaceDocument,
    request: NamespaceRemoveRowRequest, selection: NamespaceRemoveRowSelection,
    refusal: NamespaceRemoveRowRefusal, id: NamespaceRowId, id_field: namespace_id,
    handle_field: name, candidates: namespace_ids, name: |document: &NamespaceDocument| document.name.clone()
}
pub fn select_service_removal(
    cluster_id: &crate::ids::ClusterId,
    rows: Vec<StoredRow>,
    request: &ServiceRemoveRowRequest,
) -> Result<ServiceRemoveRowSelection, ServiceRemoveRowRefusal> {
    let accepted = read_rows::<ServiceDocument>(cluster_id, rows.clone()).accepted;
    if let Some(requested_id) = request.service_id.as_ref() {
        let selected = accepted
            .into_iter()
            .find(|row| row.source.key == requested_id.as_str());
        let Some(selected) = selected else {
            return if rows.iter().any(|row| row.key == requested_id.as_str()) {
                Err(ServiceRemoveRowRefusal::StoredRowUnselectable {
                    service_id: requested_id.clone(),
                })
            } else {
                Ok(ServiceRemoveRowSelection::AlreadyAbsent {
                    service_id: requested_id.clone(),
                })
            };
        };
        if selected.value.namespace_id != request.namespace_id
            || selected.value.name != request.name
        {
            return Err(ServiceRemoveRowRefusal::IdentityMismatch {
                service_id: requested_id.clone(),
                requested_namespace_id: request.namespace_id.clone(),
                requested_name: request.name.clone(),
                found_namespace_id: selected.value.namespace_id,
                found_name: selected.value.name,
            });
        }
        return Ok(ServiceRemoveRowSelection::Delete {
            service_id: requested_id.clone(),
            stored_document: selected.source.document,
        });
    }

    let mut candidates = accepted
        .into_iter()
        .filter(|row| {
            row.value.namespace_id == request.namespace_id && row.value.name == request.name
        })
        .map(|row| {
            (
                ServiceRowId::try_new(row.source.key)
                    .expect("accepted service row ids are canonical"),
                row.source.document,
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    match candidates.as_slice() {
        [] => Err(ServiceRemoveRowRefusal::NotFound {
            namespace_id: request.namespace_id.clone(),
            name: request.name.clone(),
        }),
        [(service_id, stored_document)] => Ok(ServiceRemoveRowSelection::Delete {
            service_id: service_id.clone(),
            stored_document: stored_document.clone(),
        }),
        [_, _, ..] => Err(ServiceRemoveRowRefusal::Ambiguous {
            namespace_id: request.namespace_id.clone(),
            name: request.name.clone(),
            service_ids: candidates
                .into_iter()
                .map(|(service_id, _)| service_id)
                .collect(),
        }),
    }
}

pub fn select_route_removal(
    cluster_id: &crate::ids::ClusterId,
    rows: Vec<StoredRow>,
    request: &RouteRemoveRequest,
) -> Result<RouteRemoveSelection, RouteRemoveRefusal> {
    let accepted = read_rows::<RouteBindingDocument>(cluster_id, rows.clone()).accepted;
    match select_named(
        &rows,
        accepted,
        request.hostname.as_str(),
        request.route_id.as_ref(),
        |id| RouteBindingRowId::try_new(id).expect("accepted row ids are canonical"),
        RouteBindingRowId::as_str,
        |document| document.hostname.as_str().to_owned(),
    ) {
        Selection::Delete {
            id,
            stored_document,
        } => Ok(RouteRemoveSelection::Delete {
            route_id: id,
            stored_document,
        }),
        Selection::AlreadyAbsent { id } => Ok(RouteRemoveSelection::AlreadyAbsent { route_id: id }),
        Selection::NotFound => Err(RouteRemoveRefusal::NotFound {
            hostname: request.hostname.clone(),
        }),
        Selection::Ambiguous(route_ids) => Err(RouteRemoveRefusal::Ambiguous {
            hostname: request.hostname.clone(),
            route_ids,
        }),
        Selection::NameMismatch { id, found } => Err(RouteRemoveRefusal::NameMismatch {
            route_id: id,
            requested: request.hostname.clone(),
            found: RouteHostname::try_new(found).expect("accepted route hostname remains valid"),
        }),
        Selection::StoredRowUnselectable { id } => {
            Err(RouteRemoveRefusal::StoredRowUnselectable { route_id: id })
        }
    }
}
