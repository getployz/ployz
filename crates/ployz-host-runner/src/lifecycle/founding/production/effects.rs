use super::*;

impl<R: HostRunnerCommandRunner> LinuxFoundingHostEffects<R> {
    fn profile(&mut self) -> Result<&HostPlatformProfile, FailureMessage> {
        if self.profile.is_none() {
            let release = self.runner.read_os_release()?;
            self.profile = Some(detect_host_platform(&release).map_err(failure)?);
        }
        Ok(self.profile.as_ref().expect("profile was populated"))
    }

    fn require(&mut self, program: &str, args: &[&str]) -> Result<(), FailureMessage> {
        let output = self.runner.command(program, args)?;
        if output.success {
            Ok(())
        } else {
            Err(failure(output.failure))
        }
    }

    fn install_artifact(
        &mut self,
        kind: ArtifactKind,
        spec: &ployz_core::install::InstallArtifactSpec,
    ) -> Result<(), FailureMessage> {
        let target = artifact_target(kind, spec).map_err(failure)?;
        let verified = match target.source_view() {
            ArtifactSourceView::LocalPath(path) => {
                verify_artifact_file(path, &target.digest).map_err(failure)?
            }
            ArtifactSourceView::RemoteUrl(url) => {
                let downloads = self.state.path().join("downloads");
                acquire_remote_artifact_content_addressed(
                    url,
                    &target.digest,
                    &downloads,
                    |staged| self.runner.download(url, staged),
                )
                .map_err(failure)?
            }
        };
        install_verified_artifact(&verified, &target).map_err(failure)?;
        Ok(())
    }

    fn supervisor_backend(&mut self) -> Result<SupervisorBackend, FailureMessage> {
        Ok(self.profile()?.supervisor().into())
    }

    fn env_contents(&self, include_bootstrap: bool) -> Result<Vec<u8>, FailureMessage> {
        let request = self.request.request();
        let MachineTransport::Wireguard { addr_v6, .. } = &request.machine.transport else {
            return Err(failure("founding machine transport is not WireGuard"));
        };
        let mut env = format!(
            "PLOYZ_CORROSION_API_ADDR=127.0.0.1:{CORROSION_API_PORT}\nPLOYZ_CORROSION_BEARER_TOKEN={}\nPLOYZ_CLUSTER_ID={}\nPLOYZ_MACHINE_ID={}\nPLOYZ_API_LISTEN_ADDR=[{addr_v6}]:{API_PORT}\nPLOYZ_BUILD={}\nPLOYZ_WIREGUARD_PRIVATE_KEY_PATH={}/{}\nPLOYZ_CORROSION_VERSION={}\n",
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
        if !self.runner.docker_is_installed() {
            let install = self.profile()?.docker_install();
            match install {
                crate::DockerInstall::GetDocker => {
                    let script = self.state.path().join("get-docker.sh");
                    self.runner.download("https://get.docker.com", &script)?;
                    self.require("sh", &[script.to_string_lossy().as_ref()])?;
                }
                crate::DockerInstall::AlpinePackages => {
                    self.require("apk", &["add", "docker"])?;
                }
                crate::DockerInstall::ArchPackages => {
                    self.require("pacman", &["--noconfirm", "-S", "docker"])?;
                }
                crate::DockerInstall::SusePackages => {
                    self.require("zypper", &["--non-interactive", "install", "docker"])?;
                }
                crate::DockerInstall::AmazonPackages => {
                    self.require("dnf", &["install", "-y", "docker"])?;
                }
                crate::DockerInstall::RhelRepositoryFile
                | crate::DockerInstall::CentosRepositoryFile => {
                    self.require("dnf", &["install", "-y", "docker-ce"])?;
                }
            }
        }
        let backend = self.supervisor_backend()?;
        for (program, args) in backend.docker_commands(SupervisorChange::InstallAndStart) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        self.runner.docker_info()
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
                let profile = self.profile()?.clone();
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
        let corrosion = format!(
            "[db]\npath = {db:?}\nschema_paths = [{schema:?}]\nsubscriptions_path = {subscriptions:?}\n\n[gossip]\naddr = {gossip:?}\nbootstrap = []\nplaintext = true\nmax_mtu = 1232\n\n[api]\naddr = {api:?}\nauthz.bearer-token = {token:?}\n\n[admin]\npath = {admin:?}\n",
            db = self.state.path().join("corrosion.db").display().to_string(),
            schema = self.artifacts.corrosion_schema.install_path.as_str(),
            subscriptions = subscriptions.display().to_string(),
            gossip = format!("[{addr_v6}]:{CORROSION_GOSSIP_PORT}"),
            api = format!("127.0.0.1:{CORROSION_API_PORT}"),
            token = self.corrosion_token,
            admin = self
                .state
                .path()
                .join("corrosion-admin.sock")
                .display()
                .to_string(),
        );
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
        let backend = self.supervisor_backend()?;
        for (program, args) in backend.docker_commands(SupervisorChange::Restart) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        self.runner.docker_info()
    }

    fn install_units_and_enable_ready_roles(&mut self) -> Result<(), FailureMessage> {
        let backend = self.supervisor_backend()?;
        let ployzd =
            artifact_target(ArtifactKind::Ployzd, &self.artifacts.ployzd).map_err(failure)?;
        let environment =
            PloyzdRoleEnvironmentFile::new(self.state.path().join(ENV_FILE)).map_err(failure)?;
        for role in [
            PloyzdRole::Keeper,
            PloyzdRole::Api,
            PloyzdRole::Gateway,
            PloyzdRole::Dns,
        ] {
            let spec = SupervisorUnitSpec::PloyzdRole {
                role,
                artifact: ployzd.clone(),
                environment_file: environment.clone(),
            };
            let rendered = backend.render(&spec).map_err(failure)?;
            write_durable_file(
                self.supervisor_directories.directory(backend),
                rendered.file_name(),
                FileMode::Executable0755,
                rendered.contents().as_bytes(),
            )?;
            let target = spec.target();
            let changes: &[SupervisorChange] = match founding_role_disposition(role) {
                FoundingRoleDisposition::Enabled => &[SupervisorChange::Enable],
                FoundingRoleDisposition::DisabledAndInactive => {
                    &[SupervisorChange::Disable, SupervisorChange::Stop]
                }
            };
            for change in changes {
                for (program, args) in backend.commands(*change, &target) {
                    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
                    self.require(program, &refs)?;
                }
            }
        }
        install_corrosion_unit(
            backend,
            &self.supervisor_directories,
            self.state.path().join(CORROSION_CONFIG_FILE),
        )?;
        for (program, args) in corrosion_commands(backend, CorrosionServiceChange::Enable) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        Ok(())
    }

    fn start_keeper(&mut self) -> Result<(), FailureMessage> {
        self.restart_role(PloyzdRole::Keeper)
    }

    fn start_corrosion(&mut self) -> Result<(), FailureMessage> {
        let backend = self.supervisor_backend()?;
        for (program, args) in corrosion_commands(backend, CorrosionServiceChange::Restart) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        Ok(())
    }

    fn start_api_with_bootstrap(&mut self) -> Result<(), FailureMessage> {
        self.restart_role(PloyzdRole::Api)
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
        for _ in 0..30 {
            let output = self.runner.command("wg", &["show", "ployz0", "peers"])?;
            if output.success
                && output
                    .stdout
                    .lines()
                    .any(|line| line.trim() == pubkey.as_str())
            {
                return Ok(());
            }
            thread::sleep(Duration::from_secs(1));
        }
        Err(failure(
            "Keeper did not converge the founding driver peer within 30 seconds",
        ))
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
        let backend = self.supervisor_backend()?;
        let target = crate::SupervisorUnitTarget::PloyzdRole(role);
        for (program, args) in backend.commands(SupervisorChange::Restart, &target) {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            self.require(program, &refs)?;
        }
        Ok(())
    }
}
