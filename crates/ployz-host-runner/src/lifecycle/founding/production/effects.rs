use super::*;

impl<R: HostRunnerCommandRunner> LinuxFoundingHostEffects<R> {
    fn substrate(&mut self) -> LinuxSubstrate<'_, R> {
        LinuxSubstrate::new(
            self.state.path(),
            &mut self.runner,
            &mut self.profile,
            &self.supervisor_directories,
        )
    }

    fn profile(&mut self) -> Result<HostPlatformProfile, FailureMessage> {
        Ok(self.substrate().profile()?.clone())
    }

    fn install_artifact(
        &mut self,
        kind: ArtifactKind,
        spec: &ployz_core::install::InstallArtifactSpec,
    ) -> Result<(), FailureMessage> {
        self.substrate().install_artifact(kind, spec)
    }

    fn join_substrate(&self) -> Result<JoinMachineSubstrate, FailureMessage> {
        let ployz_version =
            ExactPloyzVersion::try_new(self.artifacts.ployzd.version.as_str()).map_err(failure)?;
        JoinMachineSubstrate::try_new(
            ployz_version,
            self.corrosion_embedded_version.clone(),
            vec![
                self.artifacts.ployzd.clone(),
                self.artifacts.ebpf_bytecode.clone(),
                self.artifacts.ebpf_ctl.clone(),
                self.artifacts.corrosion.clone(),
                self.artifacts.corrosion_schema.clone(),
                self.artifacts.railpack.clone(),
            ],
        )
        .map_err(failure)
    }

    pub(super) fn persist_join_substrate(&self) -> Result<(), FailureMessage> {
        let bytes = serde_json::to_vec_pretty(&self.join_substrate()?).map_err(failure)?;
        write_durable_file(
            self.state.path(),
            JOIN_SUBSTRATE_FILE,
            FileMode::Secret0600,
            &bytes,
        )
    }

    pub(super) fn env_contents(&self, include_bootstrap: bool) -> Result<Vec<u8>, FailureMessage> {
        let request = self.request.request();
        let MachineTransport::Wireguard { addr_v6, .. } = &request.machine.transport else {
            return Err(failure("founding machine transport is not WireGuard"));
        };
        let mut env = format!(
            "PLOYZ_CORROSION_API_ADDR=127.0.0.1:{CORROSION_API_PORT}\nPLOYZ_CORROSION_BEARER_TOKEN={}\nPLOYZ_CLUSTER_ID={}\nPLOYZ_MACHINE_ID={}\nPLOYZ_API_LISTEN_ADDR=[{addr_v6}]:{API_PORT}\nPLOYZ_API_DOOR_LISTEN_ADDR=[::]:{JOIN_DOOR_PORT}\nPLOYZ_API_DOOR_PRIVATE_KEY_PATH={DOOR_PRIVATE_KEY_PATH}\nPLOYZ_API_DOOR_CERTIFICATE_PATH={DOOR_CERTIFICATE_PATH}\nPLOYZ_API_DOOR_FINGERPRINT_PATH={DOOR_FINGERPRINT_PATH}\nPLOYZ_API_JOIN_SUBSTRATE_PATH={JOIN_SUBSTRATE_PATH}\nPLOYZ_BUILD={}\nPLOYZ_WIREGUARD_PRIVATE_KEY_PATH={}/{}\nPLOYZ_CORROSION_VERSION={}\n",
            self.corrosion_token,
            request.cluster_id,
            request.machine_id,
            self.artifacts.ployzd.version.as_str(),
            self.state.path().display(),
            WIREGUARD_KEY_FILE,
            self.corrosion_embedded_version,
        );
        if include_bootstrap {
            env.push_str("PLOYZ_API_BOOTSTRAP_SECRET=");
            env.push_str(self.bootstrap_credential.as_str());
            env.push('\n');
        }
        Ok(env.into_bytes())
    }
}

impl<R: HostRunnerCommandRunner> FoundingHostEffects for LinuxFoundingHostEffects<R> {
    fn stage_exact_ployz_and_corrosion(&mut self) -> Result<(), FailureMessage> {
        for (kind, spec) in [
            (ArtifactKind::Ployzd, self.artifacts.ployzd.clone()),
            (ArtifactKind::Corrosion, self.artifacts.corrosion.clone()),
            (
                ArtifactKind::CorrosionSchema,
                self.artifacts.corrosion_schema.clone(),
            ),
            (
                ArtifactKind::EbpfBytecode,
                self.artifacts.ebpf_bytecode.clone(),
            ),
            (ArtifactKind::EbpfCtl, self.artifacts.ebpf_ctl.clone()),
        ] {
            self.install_artifact(kind, &spec)?;
        }
        let version = self
            .runner
            .command("/usr/local/bin/corrosion", &["--version"])?;
        if version.success && version.stdout.trim() == self.corrosion_embedded_version {
            Ok(())
        } else {
            Err(failure(format!(
                "installed Corrosion version mismatch: expected {:?}, got {:?}",
                self.corrosion_embedded_version,
                version.stdout.trim()
            )))
        }
    }

    fn ensure_docker(&mut self) -> Result<(), FailureMessage> {
        self.substrate().ensure_docker()
    }

    fn ensure_machine_identity_and_wireguard(&mut self) -> Result<(), FailureMessage> {
        let bytes = serde_json::to_vec_pretty(&self.machine_seed).map_err(failure)?;
        write_durable_file(
            self.state.path(),
            MACHINE_SEED_FILE,
            FileMode::Secret0600,
            &bytes,
        )?;
        write_durable_file(
            self.state.path(),
            WIREGUARD_KEY_FILE,
            FileMode::Secret0600,
            format!("{}\n", self.machine_seed.private_key).as_bytes(),
        )
    }

    fn ensure_cluster_door_material(&mut self) -> Result<(), FailureMessage> {
        let door_material = match read_door_material_state(&self.state).map_err(failure)? {
            DoorMaterialState::Complete(door_material) => door_material,
            DoorMaterialState::Incomplete => {
                for path in [
                    self.state.path().join(DOOR_KEY_FILE),
                    self.state.path().join(DOOR_CERTIFICATE_FILE),
                    self.state.path().join(DOOR_FINGERPRINT_FILE),
                ] {
                    match fs::remove_file(path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(failure(error)),
                    }
                }
                generate_door_material()?
            }
        };
        write_durable_file(
            self.state.path(),
            DOOR_KEY_FILE,
            FileMode::Secret0600,
            door_material.private_key_pem.as_bytes(),
        )?;
        write_durable_file(
            self.state.path(),
            DOOR_CERTIFICATE_FILE,
            FileMode::Plain,
            door_material.certificate_pem.as_bytes(),
        )?;
        write_durable_file(
            self.state.path(),
            DOOR_FINGERPRINT_FILE,
            FileMode::Plain,
            format!("{}\n", door_material.fingerprint).as_bytes(),
        )
    }

    fn prepare_selected_storage(&mut self) -> Result<(), FailureMessage> {
        match self.request.request().machine.storage.mode {
            StorageMode::Plain => {
                fs::create_dir_all(self.state.path().join("volumes")).map_err(failure)
            }
            StorageMode::Zfs => {
                let Some(pool) = self.zfs_pool.clone() else {
                    return Err(failure(
                        "ZFS founding request has no retained pool selection",
                    ));
                };
                let profile = self.profile()?;
                prepare_storage(
                    &mut self.runner,
                    &profile,
                    &PoolSelection::Explicit(pool),
                    self.state.path(),
                    Path::new("/etc/systemd/system/docker.service.d"),
                )
                .map(|_| ())
                .map_err(failure)
            }
        }
    }

    fn write_configuration_with_bootstrap(&mut self) -> Result<(), FailureMessage> {
        let request = self.request.request();
        let MachineTransport::Wireguard { addr_v6, .. } = &request.machine.transport else {
            return Err(failure("founding machine transport is not WireGuard"));
        };
        self.persist_join_substrate()?;
        write_durable_file(
            self.state.path(),
            CORROSION_TOKEN_FILE,
            FileMode::Secret0600,
            format!("{}\n", self.corrosion_token).as_bytes(),
        )?;
        write_durable_file(
            self.state.path(),
            BOOTSTRAP_CREDENTIAL_FILE,
            FileMode::Secret0600,
            format!("{}\n", self.bootstrap_credential.as_str()).as_bytes(),
        )?;
        let subscriptions = self.state.path().join("subscriptions");
        fs::create_dir_all(&subscriptions).map_err(failure)?;
        let corrosion = render_corrosion_config(CorrosionConfig {
            state: self.state.path(),
            schema_path: self.artifacts.corrosion_schema.install_path.as_str(),
            gossip_addr: *addr_v6,
            bootstrap: CorrosionBootstrap::Founder,
            bearer_token: &self.corrosion_token,
        });
        write_durable_file(
            self.state.path(),
            CORROSION_CONFIG_FILE,
            FileMode::Secret0600,
            corrosion.as_bytes(),
        )?;
        write_durable_file(
            self.state.path(),
            ENV_FILE,
            FileMode::Secret0600,
            &self.env_contents(true)?,
        )?;
        merge_docker_daemon_config(&request.cluster.prefix)
    }

    fn persist_validated_founding_request(&mut self) -> Result<(), FailureMessage> {
        match self.request.request().machine.storage.mode {
            StorageMode::Plain => {}
            StorageMode::Zfs => {
                let Some(pool) = &self.zfs_pool else {
                    return Err(failure(
                        "ZFS founding request has no retained pool selection",
                    ));
                };
                write_durable_file(
                    self.state.path(),
                    ZFS_POOL_FILE,
                    FileMode::Secret0600,
                    format!("{}\n", pool.as_str()).as_bytes(),
                )?;
            }
        }
        let bytes = serde_json::to_vec_pretty(self.request.request()).map_err(failure)?;
        write_durable_file(
            self.state.path(),
            FOUNDING_REQUEST_FILE,
            FileMode::Secret0600,
            &bytes,
        )
    }

    fn restart_and_verify_docker_configuration(&mut self) -> Result<(), FailureMessage> {
        self.substrate().restart_docker_and_verify()
    }

    fn install_units_and_enable_ready_roles(&mut self) -> Result<(), FailureMessage> {
        let ployzd =
            artifact_target(ArtifactKind::Ployzd, &self.artifacts.ployzd).map_err(failure)?;
        let environment =
            PloyzdRoleEnvironmentFile::new(self.state.path().join(ENV_FILE)).map_err(failure)?;
        let corrosion_config = self.state.path().join(CORROSION_CONFIG_FILE);
        {
            let mut substrate = self.substrate();
            substrate.install_ployzd_units(&ployzd, &environment)?;
            substrate.install_corrosion_unit(&corrosion_config)?;
            substrate.change_corrosion_service(CorrosionServiceChange::Enable)?;
        }
        Ok(())
    }

    fn start_keeper(&mut self) -> Result<(), FailureMessage> {
        self.restart_role(PloyzdRole::Keeper)
    }

    fn start_corrosion(&mut self) -> Result<(), FailureMessage> {
        self.substrate()
            .change_corrosion_service(CorrosionServiceChange::Restart)
    }

    fn start_api_with_bootstrap(&mut self) -> Result<(), FailureMessage> {
        self.restart_role(PloyzdRole::Api)
    }

    fn enable_and_start_dns(&mut self) -> Result<(), FailureMessage> {
        self.substrate().enable_and_start_dns()
    }

    fn await_dns_readiness(&mut self) -> Result<(), FailureMessage> {
        let gateway = machine_endpoint_gateway(&self.request.request().machine.transport);
        self.substrate().await_dns_readiness(gateway)
    }

    fn await_driver_peer_convergence(
        &mut self,
        driver: &FoundingDriverEnrollment,
    ) -> Result<(), FailureMessage> {
        let Some((_peer_id, document)) = driver.enrolled_peer() else {
            return Ok(());
        };
        let PeerTransport::Wireguard { pubkey, .. } = &document.transport else {
            return Err(failure("founding driver transport is not WireGuard"));
        };
        let deadline = Instant::now() + DRIVER_PEER_CONVERGENCE_TIMEOUT;
        loop {
            let output = self.runner.command("wg", &["show", "ployz0", "peers"])?;
            if output.success
                && output
                    .stdout
                    .lines()
                    .any(|line| line.trim() == pubkey.as_str())
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
        Err(failure(format!(
            "Keeper did not converge the founding driver peer within {} seconds",
            DRIVER_PEER_CONVERGENCE_TIMEOUT.as_secs()
        )))
    }

    fn remove_bootstrap_credential(&mut self) -> Result<(), FailureMessage> {
        write_durable_file(
            self.state.path(),
            ENV_FILE,
            FileMode::Secret0600,
            &self.env_contents(false)?,
        )?;
        match fs::remove_file(self.state.path().join(BOOTSTRAP_CREDENTIAL_FILE)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(failure(error)),
        }
    }

    fn restart_api_without_bootstrap(&mut self) -> Result<(), FailureMessage> {
        self.restart_role(PloyzdRole::Api)
    }
}

impl<R: HostRunnerCommandRunner> LinuxFoundingHostEffects<R> {
    fn restart_role(&mut self, role: PloyzdRole) -> Result<(), FailureMessage> {
        let target = crate::SupervisorUnitTarget::PloyzdRole(role);
        self.substrate()
            .run_supervisor(crate::SupervisorChange::Restart, &target)
    }
}
