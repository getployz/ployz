use std::time::Duration;

use ployz_core::{
    AdvertisedEndpoint, ApproveJoinRequest, Claim, ConfirmJoinRequest, EnrollAs, EnrollRequest,
    ForgetRequest, Forgotten, InspectRequest, JOIN_TRUST_CAPABILITY, JoinPin, ListMachinesRequest,
    LocalMachinePhase, MACHINE_START_WAIT, Machine, MachineId, MachineName, MachineToken,
    MachineUpdate, PendingJoinsRequest, RegisterRequest, Registered, RttObservation, Sealing,
    SetDeviceRequest, TokenRequest, UpdateMachineRequest,
    authority::{JoinApproval, Signed, SignerKey, Timestamp, join::JoinConfirmation},
    op,
};

use super::{
    CLUSTER_NETWORK, Cluster, TestkitError, command_error, docker_output, poll_until, response,
};

const WIREGUARD_INTERFACE: &str = "ployz-wg";

/// A Machine container's WireGuard device, with keys and addresses as `wg` prints them.
#[derive(Debug)]
pub struct WireGuardDump {
    pub public_key: String,
    pub listen_port: u16,
    pub peers: Vec<WireGuardDumpPeer>,
}

#[derive(Debug)]
pub struct WireGuardDumpPeer {
    pub public_key: String,
    pub allowed_ips: Vec<String>,
}

/// A join its entry Machine admitted under the Cluster's Device. The joiner enrolls with
/// `registration`, then [`Cluster::confirm`] relays its confirmation back through `request`.
#[derive(Clone, Debug)]
pub struct Admission {
    entry: usize,
    request: RegisterRequest,
    pub registration: Registered,
}

impl Cluster {
    /// Admit Machine 1 through Machine 0 as `machine-2`.
    pub async fn register_second(&self) -> Result<Admission, TestkitError> {
        let token = self.machine_token(1, vec![self.endpoint(1)?]).await?;
        self.register_machine(0, "machine-2", token).await
    }

    /// Join Machine `target` through Machine `entry` as `ployz server add` does: the entry
    /// admits it under the Cluster's Device, the joiner enrolls pinned to that Device, and
    /// its confirmation goes back through the entry.
    pub async fn add_machine(
        &self,
        entry: usize,
        target: usize,
        name: &str,
    ) -> Result<Machine, TestkitError> {
        let token = self
            .machine_token(target, vec![self.endpoint(target)?])
            .await?;
        let admission = self.register_machine(entry, name, token).await?;
        let confirmation = self
            .join(
                target,
                EnrollRequest {
                    role: EnrollAs::Member {
                        registration: admission.registration.clone(),
                        pin: Some(JoinPin {
                            devices: vec![self.device()],
                        }),
                    },
                    wireguard_mtu: None,
                    pinned: Default::default(),
                },
            )
            .await?;
        self.confirm(&admission, target, confirmation).await?;
        self.wait_phase(
            target,
            LocalMachinePhase::Participating,
            Duration::from_secs(120),
        )
        .await?;
        Ok(admission.registration.assigned_machine)
    }

    /// Relay joiner `target`'s `confirmation` to its admitting Machine, which publishes the
    /// joiner's row and answers with the restrictions the joiner takes before it serves.
    pub async fn confirm(
        &self,
        admission: &Admission,
        target: usize,
        confirmation: Option<JoinConfirmation>,
    ) -> Result<(), TestkitError> {
        let confirmation = confirmation
            .ok_or_else(|| TestkitError::Rpc("the joiner answered no join confirmation".into()))?;
        let answered = self
            .register(
                admission.entry,
                RegisterRequest {
                    join_confirmation: Some(confirmation),
                    ..admission.request.clone()
                },
            )
            .await?;
        let restrictions = answered.restrictions.ok_or_else(|| {
            TestkitError::Rpc("the admitting Machine answered no join restrictions".into())
        })?;
        let request =
            op::ConfirmJoin::into_request(ConfirmJoinRequest { restrictions }).encode()?;
        // The joiner's daemon restarts after it enrolls, so this dials it until it answers.
        poll_until(
            MACHINE_START_WAIT,
            "the joiner takes its restrictions",
            async || {
                let mut client = self.client(target).await?;
                response(client.confirm_join(request.clone()).await?.into_inner())
            },
        )
        .await
        .map(drop)
        .map_err(|_| self.readiness_timeout(target))
    }

    /// Register `token` through Machine `entry` as `name`. A join waiting for approval is
    /// approved with the Cluster's Device, after the entry is told to trust it, and sent again.
    pub(super) async fn register_machine(
        &self,
        entry: usize,
        name: &str,
        token: MachineToken,
    ) -> Result<Admission, TestkitError> {
        let mut client = self.client(entry).await?;
        let snapshot = response(
            client
                .list_machines(op::ListMachines::into_request(ListMachinesRequest {}).encode()?)
                .await?
                .into_inner(),
        )?
        .decode::<op::ListMachines>()?
        .enrollment
        .ok_or_else(|| TestkitError::Rpc("missing enrollment snapshot".into()))?;
        // The joiner takes its restrictions from ConfirmJoin, which `confirm` relays.
        let capabilities = (token.runtime.claim(JOIN_TRUST_CAPABILITY) == Claim::Advertises)
            .then(|| JOIN_TRUST_CAPABILITY.to_owned())
            .into_iter()
            .collect();
        let mut request = RegisterRequest {
            join_approval: None,
            join_confirmation: None,
            machine_id: token.id,
            assigned_subnet: None,
            initial_policy: Default::default(),
            name: MachineName::parse(name)?,
            storage: ployz_core::StorageChoice::None,
            public_key: token.public_key,
            public_ip: token.public_ip,
            advertised_endpoints: token.advertised_endpoints,
            runtime: token.runtime,
            capabilities,
            first_key: None,
        };
        let assignment = ployz_core::allocate_enrollment(&request, &snapshot, &[])
            .map_err(|error| TestkitError::Rpc(error.to_string()))?;
        request.assigned_subnet = Some(assignment.machine.subnet);
        self.approve_device(entry).await?;
        let registration = match self.register(entry, request.clone()).await {
            Ok(registration) => registration,
            Err(refused) => {
                if !self.approve_waiting_join(entry, &request).await? {
                    return Err(refused);
                }
                self.register(entry, request.clone()).await?
            }
        };
        Ok(Admission {
            entry,
            request,
            registration,
        })
    }

    async fn register(
        &self,
        entry: usize,
        request: RegisterRequest,
    ) -> Result<Registered, TestkitError> {
        let mut client = self.client(entry).await?;
        Ok(response(
            client
                .register(op::Register::into_request(request).encode()?)
                .await?
                .into_inner(),
        )?
        .decode::<op::Register>()?)
    }

    /// Have Machine `entry` trust the Cluster's Device, as `ployz init` approves the
    /// person's own. Approving it again changes nothing.
    async fn approve_device(&self, entry: usize) -> Result<(), TestkitError> {
        let mut client = self.client(entry).await?;
        response(
            client
                .set_device(
                    op::SetDevice::into_request(SetDeviceRequest::Approve { key: self.device() })
                        .encode()?,
                )
                .await?
                .into_inner(),
        )?;
        Ok(())
    }

    /// Sign the join waiting on Machine `entry` for `request` with the Cluster's Device and
    /// hand it back, as `ployz server add` does. False when no join waits for it.
    async fn approve_waiting_join(
        &self,
        entry: usize,
        request: &RegisterRequest,
    ) -> Result<bool, TestkitError> {
        let mut client = self.client(entry).await?;
        let pending = response(
            client
                .pending_joins(op::PendingJoins::into_request(PendingJoinsRequest {}).encode()?)
                .await?
                .into_inner(),
        )?
        .decode::<op::PendingJoins>()?;
        let Some(mut body) = pending
            .joins
            .into_iter()
            .find(|body| body.machine.id == request.machine_id)
        else {
            return Ok(false);
        };
        if !body.machine.matches_request(request) {
            return Err(TestkitError::Rpc(
                "the waiting join does not match the registration".into(),
            ));
        }
        body.signer = SignerKey::Device(self.device());
        let approval = Signed::sign(&body, &self.device)
            .map_err(|error| TestkitError::Rpc(error.to_string()))?;
        response(
            client
                .approve_join(
                    op::ApproveJoin::into_request(ApproveJoinRequest { approval }).encode()?,
                )
                .await?
                .into_inner(),
        )?;
        Ok(true)
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

    pub async fn forget(&self, entry: usize, machine_id: MachineId) -> Result<(), TestkitError> {
        let mut client = self.client(entry).await?;
        match response(
            client
                .forget(op::Forget::into_request(ForgetRequest { machine_id }).encode()?)
                .await?
                .into_inner(),
        )?
        .decode::<op::Forget>()?
        {
            Forgotten::Forgotten => Ok(()),
            Forgotten::Refused { error, .. } => Err(TestkitError::Rpc(error.message)),
        }
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
        .rtts)
    }

    pub(super) async fn machine_token(
        &self,
        index: usize,
        advertised_endpoints: Vec<AdvertisedEndpoint>,
    ) -> Result<MachineToken, TestkitError> {
        let mut client = self.client(index).await?;
        response(
            client
                .inspect(
                    op::Inspect::into_request(InspectRequest {
                        advertised_endpoints,
                        token: Some(TokenRequest::default()),
                        ..Default::default()
                    })
                    .encode()?,
                )
                .await?
                .into_inner(),
        )?
        .decode::<op::Inspect>()?
        .token
        .ok_or_else(|| TestkitError::Rpc("this Server's Ployz does not report a token".into()))
    }

    /// Read one Machine container's WireGuard device with `wg show dump`.
    pub fn wireguard_dump(&self, index: usize) -> Result<WireGuardDump, TestkitError> {
        let output = docker_output([
            "exec",
            &self.container_name(index)?,
            "wg",
            "show",
            WIREGUARD_INTERFACE,
            "dump",
        ])?;
        if !output.status.success() {
            return Err(command_error(output));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let unreadable = || TestkitError::DockerCommand(format!("unreadable wg dump: {text}"));
        let mut lines = text
            .lines()
            .map(|line| line.split('\t').collect::<Vec<_>>());
        let device = lines.next().ok_or_else(unreadable)?;
        let [_, public_key, listen_port, ..] = device.as_slice() else {
            return Err(unreadable());
        };
        let listen_port = listen_port.parse().map_err(|_| unreadable())?;
        let peers = lines
            .map(|peer| match peer.as_slice() {
                [public_key, _, _, allowed_ips, ..] => Ok(WireGuardDumpPeer {
                    public_key: (*public_key).to_owned(),
                    allowed_ips: allowed_ips
                        .split(',')
                        .filter(|address| *address != "(none)")
                        .map(str::to_owned)
                        .collect(),
                }),
                _ => Err(unreadable()),
            })
            .collect::<Result<_, _>>()?;
        Ok(WireGuardDump {
            public_key: (*public_key).to_owned(),
            listen_port,
            peers,
        })
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
                r#"[{{"query":"INSERT INTO containers (id, container, machine_id) VALUES (?, '{{}}', ?)","params":["{container_id}","{machine_id}"]}}]"#,
            ),
        )
    }

    /// Seed, on Machine `index` alone, a row copying `source` under `id` and `name`. Its
    /// approval is signed by the Cluster's Device, as every admitted Member's row carries
    /// one, so a Machine lists it.
    pub async fn seed_machine_collision(
        &self,
        index: usize,
        source: &Machine,
        id: &MachineId,
        name: &MachineName,
    ) -> Result<(), TestkitError> {
        let Some(Sealing::Held {
            public_key: cluster,
        }) = self.inspect(index).await?.sealing
        else {
            return Err(TestkitError::Rpc("the Machine holds no Sealing Key".into()));
        };
        let row = Machine {
            id: *id,
            name: name.clone(),
            ..source.clone()
        };
        let issued_at = Timestamp::now();
        let body = JoinApproval {
            cluster,
            machine: (&row).into(),
            network: CLUSTER_NETWORK.parse().expect("static network is valid"),
            sealing_recipient: row.public_key,
            devices: vec![self.device()],
            passkeys: Vec::new(),
            grants: Vec::new(),
            admitting_machines: vec![source.id],
            bootstrap: Vec::new(),
            issued_at,
            expires_at: issued_at.plus(60 * 60 * 1000),
            signer: SignerKey::Device(self.device()),
            proposal: None,
        };
        let approval = serde_json::to_string(
            &Signed::sign(&body, &self.device)
                .map_err(|error| TestkitError::Rpc(error.to_string()))?,
        )
        .map_err(|error| TestkitError::Rpc(error.to_string()))?;
        let query = serde_json::json!([{
            "query": "INSERT INTO machines (id, info) SELECT ?, json_set(info, '$.id', ?, '$.name', ?, '$.join_approval', json(?)) FROM machines WHERE id = ?",
            "params": [id, id, name, approval, source.id],
        }]);
        self.corrosion_transaction(index, &query.to_string())
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
