use std::time::Duration;

use ployz_core::{
    AdvertisedEndpoint, InspectRequest, InspectWireGuardRequest, JoinRequest, ListMachinesRequest,
    LocalMachinePhase, Machine, MachineId, MachineName, MachineToken, MachineTokenRequest,
    MachineUpdate, RegisterRequest, RemoveMachineRequest, RttObservation, UpdateMachineRequest,
    WireGuardDevice, WireGuardPublicKey, op,
};

use super::{Cluster, TestkitError, docker, response};

impl Cluster {
    pub async fn register_second(&self) -> Result<ployz_core::Registered, TestkitError> {
        let token = self.machine_token(1, vec![self.endpoint(1)?]).await?;
        self.register_machine(0, "machine-2", token).await
    }

    pub async fn add_machine(
        &self,
        entry: usize,
        target: usize,
        name: &str,
    ) -> Result<Machine, TestkitError> {
        let token = self
            .machine_token(target, vec![self.endpoint(target)?])
            .await?;
        let registration = self.register_machine(entry, name, token).await?;
        let assigned = registration.assigned_machine.clone();
        self.join(
            target,
            JoinRequest {
                registration,
                wireguard_mtu: None,
                cloud_pairing: None,
            },
        )
        .await?;
        self.wait_phase(
            target,
            LocalMachinePhase::Participating,
            Duration::from_secs(120),
        )
        .await?;
        Ok(assigned)
    }

    pub(super) async fn register_machine(
        &self,
        entry: usize,
        name: &str,
        token: MachineToken,
    ) -> Result<ployz_core::Registered, TestkitError> {
        let mut client = self.client(entry).await?;
        Ok(response(
            client
                .register(
                    op::Register::into_request(RegisterRequest {
                        name: MachineName::parse(name)?,
                        public_key: token.public_key,
                        public_ip: token.public_ip,
                        advertised_endpoints: token.advertised_endpoints,
                        runtime: token.runtime,
                    })
                    .encode()?,
                )
                .await?
                .into_inner(),
        )?
        .decode::<op::Register>()?)
    }

    pub async fn machines(
        &self,
        index: usize,
    ) -> Result<Vec<ployz_core::MachineObservation>, TestkitError> {
        let mut client = self.client(index).await?;
        Ok(response(
            client
                .list_machines(op::ListMachines::into_request(ListMachinesRequest {}).encode()?)
                .await?
                .into_inner(),
        )?
        .decode::<op::ListMachines>()?
        .machines)
    }

    pub async fn update_machine(
        &self,
        entry: usize,
        target: &str,
        update: MachineUpdate,
    ) -> Result<Machine, TestkitError> {
        let mut client = self.client(entry).await?;
        let mut request = tonic::Request::new(
            op::UpdateMachine::into_request(UpdateMachineRequest { update }).encode()?,
        );
        request.metadata_mut().insert(
            "machine",
            target
                .parse()
                .map_err(|_| TestkitError::Rpc("invalid Machine route".into()))?,
        );
        Ok(
            response(client.update_machine(request).await?.into_inner())?
                .decode::<op::UpdateMachine>()?
                .machine,
        )
    }

    pub async fn remove_machine(
        &self,
        entry: usize,
        machine_id: MachineId,
    ) -> Result<(), TestkitError> {
        let mut client = self.client(entry).await?;
        response(
            client
                .remove_machine(
                    op::RemoveMachine::into_request(RemoveMachineRequest { machine_id })
                        .encode()?,
                )
                .await?
                .into_inner(),
        )?
        .decode::<op::RemoveMachine>()
        .map(|_| ())
        .map_err(Into::into)
    }

    pub async fn inspect_rtts(&self, index: usize) -> Result<Vec<RttObservation>, TestkitError> {
        let mut client = self.client(index).await?;
        Ok(response(
            client
                .inspect(
                    op::Inspect::into_request(InspectRequest {
                        include_rtts: true,
                        ..Default::default()
                    })
                    .encode()?,
                )
                .await?
                .into_inner(),
        )?
        .decode::<op::Inspect>()?
        .rtts
        .clone())
    }

    pub(super) async fn machine_token(
        &self,
        index: usize,
        advertised_endpoints: Vec<AdvertisedEndpoint>,
    ) -> Result<MachineToken, TestkitError> {
        let mut client = self.client(index).await?;
        Ok(response(
            client
                .machine_token(
                    op::MachineToken::into_request(MachineTokenRequest {
                        advertised_endpoints,
                        ..Default::default()
                    })
                    .encode()?,
                )
                .await?
                .into_inner(),
        )?
        .decode::<op::MachineToken>()?)
    }

    pub async fn inspect_wireguard(&self, index: usize) -> Result<WireGuardDevice, TestkitError> {
        let mut client = self.client(index).await?;
        Ok(response(
            client
                .inspect_wireguard(
                    op::InspectWireguard::into_request(InspectWireGuardRequest {}).encode()?,
                )
                .await?
                .into_inner(),
        )?
        .decode::<op::InspectWireguard>()?
        .device)
    }

    pub fn inject_wireguard_peer(
        &self,
        index: usize,
        key: WireGuardPublicKey,
    ) -> Result<(), TestkitError> {
        docker([
            "exec",
            &self.container_name(index)?,
            "wg",
            "set",
            "ployz-wg",
            "peer",
            &key.to_string(),
            "allowed-ips",
            "203.0.113.9/32",
        ])
    }

    pub fn seed_container_row(
        &self,
        index: usize,
        container_id: &str,
        machine_id: &MachineId,
    ) -> Result<(), TestkitError> {
        self.corrosion_transaction(
            index,
            &format!(
                r#"[{{"query":"INSERT INTO containers (id, container, machine_id, updated_at) VALUES (?, '{{}}', ?, datetime('now'))","params":["{container_id}","{machine_id}"]}}]"#,
            ),
        )
    }

    pub fn seed_machine_collision(
        &self,
        index: usize,
        source: &Machine,
        id: &MachineId,
        name: &MachineName,
    ) -> Result<(), TestkitError> {
        self.corrosion_transaction(
            index,
            &format!(
                r#"[{{"query":"INSERT INTO machines (id, info, created_at, updated_at) SELECT ?, json_set(info, '$.id', ?, '$.name', ?), datetime('now'), datetime('now') FROM machines WHERE id = ?","params":["{id}","{id}","{name}","{}"]}}]"#,
                source.id
            ),
        )
    }

    pub fn replicated_row_exists(
        &self,
        index: usize,
        table: &str,
        id: &str,
    ) -> Result<bool, TestkitError> {
        if !matches!(table, "machines" | "containers") {
            return Err(TestkitError::Rpc("unsupported replicated table".into()));
        }
        let output = self.corrosion_request(
            index,
            "v1/queries",
            &format!(r#"{{"query":"SELECT id FROM {table} WHERE id = ?","params":["{id}"]}}"#),
        )?;
        Ok(String::from_utf8_lossy(&output.stdout).contains(id))
    }
}
