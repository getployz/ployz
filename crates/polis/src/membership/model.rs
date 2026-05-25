use crate::{Error, IrohEndpointId, StoreResult, StoreRow};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StoreMachineId(String);

impl StoreMachineId {
    pub fn parse(value: impl Into<String>) -> crate::Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IslandId(String);

impl IslandId {
    pub fn parse(value: impl Into<String>) -> crate::Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireGuardPublicKey(String);

impl WireGuardPublicKey {
    pub fn parse(value: impl Into<String>) -> crate::Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OverlayIp(String);

impl OverlayIp {
    pub fn parse(value: impl Into<String>) -> crate::Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowEpoch(u64);

impl RowEpoch {
    pub fn new(value: u64) -> crate::Result<Self> {
        if value == 0 || value > i64::MAX as u64 {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }

    pub(super) fn sql_value(self) -> i64 {
        self.0 as i64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MembershipLifecycle {
    Active,
    Removing,
    Tombstoned,
    Conflicted,
    Deleted,
}

impl MembershipLifecycle {
    pub fn parse(value: &str) -> crate::Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "removing" => Ok(Self::Removing),
            "tombstoned" => Ok(Self::Tombstoned),
            "conflicted" => Ok(Self::Conflicted),
            "deleted" => Ok(Self::Deleted),
            _ => Err(Error::MalformedPayload),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Removing => "removing",
            Self::Tombstoned => "tombstoned",
            Self::Conflicted => "conflicted",
            Self::Deleted => "deleted",
        }
    }
}

pub(in crate::membership) fn machine_row_from_store_row(row: &StoreRow) -> StoreResult<MachineRow> {
    let machine_id = StoreMachineId::parse(row.text("machine_id")?.to_string())
        .map_err(|_| crate::StoreError::MalformedPayload)?;
    let island_id = IslandId::parse(row.text("island_id")?.to_string())
        .map_err(|_| crate::StoreError::MalformedPayload)?;
    let iroh_endpoint_id = IrohEndpointId::parse(row.text("iroh_endpoint_id")?.to_string())
        .map_err(|_| crate::StoreError::MalformedPayload)?;
    let wireguard_public_key =
        WireGuardPublicKey::parse(row.text("wireguard_public_key")?.to_string())
            .map_err(|_| crate::StoreError::MalformedPayload)?;
    let overlay_ip = OverlayIp::parse(row.text("overlay_ip")?.to_string())
        .map_err(|_| crate::StoreError::MalformedPayload)?;
    let lifecycle = MembershipLifecycle::parse(row.text("lifecycle")?)
        .map_err(|_| crate::StoreError::MalformedPayload)?;
    let epoch = RowEpoch::new(row.integer("epoch")? as u64)
        .map_err(|_| crate::StoreError::MalformedPayload)?;

    Ok(MachineRow::new(
        machine_id,
        island_id,
        iroh_endpoint_id,
        wireguard_public_key,
        overlay_ip,
        lifecycle,
        epoch,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRow {
    pub(super) machine_id: StoreMachineId,
    pub(super) island_id: IslandId,
    pub(super) iroh_endpoint_id: IrohEndpointId,
    pub(super) wireguard_public_key: WireGuardPublicKey,
    pub(super) overlay_ip: OverlayIp,
    pub(super) lifecycle: MembershipLifecycle,
    pub(super) epoch: RowEpoch,
}

impl MachineRow {
    #[must_use]
    pub fn new(
        machine_id: StoreMachineId,
        island_id: IslandId,
        iroh_endpoint_id: IrohEndpointId,
        wireguard_public_key: WireGuardPublicKey,
        overlay_ip: OverlayIp,
        lifecycle: MembershipLifecycle,
        epoch: RowEpoch,
    ) -> Self {
        Self {
            machine_id,
            island_id,
            iroh_endpoint_id,
            wireguard_public_key,
            overlay_ip,
            lifecycle,
            epoch,
        }
    }

    #[must_use]
    pub fn machine_id(&self) -> &StoreMachineId {
        &self.machine_id
    }

    #[must_use]
    pub fn island_id(&self) -> &IslandId {
        &self.island_id
    }

    #[must_use]
    pub fn endpoint_id(&self) -> &IrohEndpointId {
        &self.iroh_endpoint_id
    }

    #[must_use]
    pub fn wireguard_public_key(&self) -> &WireGuardPublicKey {
        &self.wireguard_public_key
    }

    #[must_use]
    pub fn overlay_ip(&self) -> &OverlayIp {
        &self.overlay_ip
    }

    #[must_use]
    pub fn lifecycle(&self) -> MembershipLifecycle {
        self.lifecycle
    }

    #[must_use]
    pub fn epoch(&self) -> RowEpoch {
        self.epoch
    }
}

fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> crate::Result<T> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(Error::MalformedPayload);
    }
    Ok(build(value))
}
