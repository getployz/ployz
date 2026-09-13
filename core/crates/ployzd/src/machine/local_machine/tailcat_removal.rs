//! Bounded endpoint revocation remains admitted after the inbound stream closes.
use std::{
    io::Write,
    process::{Command, Stdio},
};

use ployz_core::{SetCloudPairingRequest, TailcatRemoval};

use super::{Error, LocalMachine};

impl LocalMachine {
    /// Set or clear Cloud Pairing, or clear it and rotate its endpoint under mutation admission.
    ///
    /// # Errors
    /// Returns admission, persistence, validation, or endpoint lifecycle failures.
    pub async fn set_cloud_pairing(&self, request: SetCloudPairingRequest) -> Result<(), Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            match request {
                SetCloudPairingRequest::Set { pairing } => {
                    local.set_cloud_pairing_admitted(Some(pairing))
                }
                SetCloudPairingRequest::Clear {} => local.set_cloud_pairing_admitted(None),
                SetCloudPairingRequest::Remove { removal } => {
                    tokio::task::spawn_blocking(move || {
                        complete_removal(
                            &local,
                            &removal,
                            crate::installer::TAILCAT_HELPER_PROGRAM,
                            "systemctl",
                        )
                    })
                    .await?
                }
            }
        })
        .await
    }
}

fn complete_removal(
    local: &LocalMachine,
    removal: &TailcatRemoval,
    helper: &str,
    systemctl: &str,
) -> Result<(), Error> {
    if local
        .record()?
        .cloud_pairing
        .as_ref()
        .is_some_and(|pairing| pairing.secret() != &removal.expected_pairing)
    {
        return Err(Error::Cleanup("stale Cloud Pairing removal".into()));
    }
    // Validate before clearing pairing; rotate rechecks under the startup lock.
    endpoint_command(helper, "validate-rotation", removal)?;
    local.lock_store()?.persist_cloud_pairing(None)?;
    endpoint_command(helper, "rotate", removal)?;
    // Always restart, including when disk already held successor. Disk equality
    // does not establish which PSK the running helper currently accepts.
    let status = Command::new(systemctl)
        .args(["restart", "ployz-tailcat.service"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| endpoint_error())?;
    if !status.success() {
        return Err(endpoint_error());
    }
    Ok(())
}

fn endpoint_error() -> Error {
    Error::Cleanup("Tailcat removal is unconfirmed: endpoint operation failed".into())
}

fn endpoint_command(helper: &str, operation: &str, removal: &TailcatRemoval) -> Result<(), Error> {
    let mut child = Command::new(helper)
        .arg(operation)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| endpoint_error())?;
    let write = (|| {
        let mut input = child.stdin.take().ok_or_else(endpoint_error)?;
        writeln!(
            input,
            "{}\n{}",
            removal.expected.as_str(),
            removal.successor.as_str()
        )
        .map_err(|_| endpoint_error())
    })();
    let status = child.wait().map_err(|_| endpoint_error())?;
    write?;
    if !status.success() {
        return Err(endpoint_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::LocalMachineStore;
    use ployz_core::{CloudPairing, TailcatCapability};
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::{Arc, Mutex},
    };

    #[test]
    fn removal_persists_before_restart_and_retries_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalMachineStore::open(dir.path()).unwrap();
        let (restart, _) = tokio::sync::watch::channel(false);
        let local = LocalMachine::new(Arc::new(Mutex::new(store)), restart);
        let pairing = CloudPairing::new(ployz_core::PairingCredential::parse("pairing").unwrap());
        local
            .lock_store()
            .unwrap()
            .persist_cloud_pairing(Some(pairing.clone()))
            .unwrap();
        let removal = TailcatRemoval {
            expected: TailcatCapability::parse("old-secret").unwrap(),
            successor: TailcatCapability::parse("new-secret").unwrap(),
            expected_pairing: pairing.secret().clone(),
        };
        // Trace persisted pairing at each process boundary, without logging stdin.
        let program = dir.path().join("command");
        fs::write(
            &program,
            r#"#!/bin/sh
cd "$(dirname "$0")" || exit 1
printf '%s ' "$1" >> trace
if grep -q '"secret"' machine.json; then echo paired >> trace; else echo cleared >> trace; fi
if [ "$1" != restart ]; then
  IFS= read -r expected
  IFS= read -r successor
  [ "$expected" = old-secret ] && [ "$successor" = new-secret ] || exit 1
fi
"#,
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        for _ in 0..2 {
            complete_removal(
                &local,
                &removal,
                program.to_str().unwrap(),
                program.to_str().unwrap(),
            )
            .unwrap();
        }
        assert!(local.record().unwrap().cloud_pairing.is_none());
        let new_pairing =
            CloudPairing::new(ployz_core::PairingCredential::parse("new-pairing").unwrap());
        local
            .lock_store()
            .unwrap()
            .persist_cloud_pairing(Some(new_pairing.clone()))
            .unwrap();
        assert!(
            complete_removal(
                &local,
                &removal,
                program.to_str().unwrap(),
                program.to_str().unwrap()
            )
            .is_err()
        );
        assert_eq!(local.record().unwrap().cloud_pairing, Some(new_pairing));
        assert_eq!(
            fs::read_to_string(dir.path().join("trace")).unwrap(),
            "validate-rotation paired\nrotate cleared\nrestart cleared\nvalidate-rotation cleared\nrotate cleared\nrestart cleared\n"
        );
    }
    #[tokio::test]
    async fn removal_rejects_repairing_and_stale_pairing_before_clearing() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalMachineStore::open(dir.path()).unwrap();
        let (reset, _) = tokio::sync::watch::channel(false);
        let local = LocalMachine::new(Arc::new(Mutex::new(store)), reset);
        let pairing = CloudPairing::new(ployz_core::PairingCredential::parse("pairing").unwrap());
        local
            .initialize(ployz_core::InitializeRequest {
                initial_policy: Default::default(),
                name: ployz_core::MachineName::parse("first").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "192.0.2.1:51820".parse().unwrap(),
                )],
                wireguard_mtu: None,
                cloud_pairing: Some(pairing.clone()),
            })
            .await
            .unwrap();
        let removal = TailcatRemoval {
            expected: TailcatCapability::parse("private-old").unwrap(),
            successor: TailcatCapability::parse("private-next").unwrap(),
            expected_pairing: ployz_core::PairingCredential::parse("stale-pairing").unwrap(),
        };
        assert!(
            serde_json::from_value::<SetCloudPairingRequest>(serde_json::json!({
                "kind": "set", "pairing": pairing, "removal": removal,
            }))
            .is_err()
        );
        assert_eq!(local.record().unwrap().cloud_pairing, Some(pairing.clone()));
        let error = local
            .set_cloud_pairing(SetCloudPairingRequest::Remove { removal })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("stale Cloud Pairing"));
        assert_eq!(local.record().unwrap().cloud_pairing, Some(pairing.clone()));
        let error = serde_json::from_value::<SetCloudPairingRequest>(serde_json::json!({
            "kind": "remove", "removal": {
                "expected_pairing": pairing.secret(), "expected": "private-old\nextra-command", "successor": "private-next",
            },
        })).unwrap_err();
        assert!(!error.to_string().contains("private-old"));
        assert_eq!(local.record().unwrap().cloud_pairing, Some(pairing));
    }
}
