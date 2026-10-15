//! Where the observe process pushes crash events, as an admitted mutation.

use std::net::IpAddr;

use ployz_core::{PushTargetSet, SetPushTargetRequest};
use reqwest::Url;
use url::Host;

use super::{Error, LocalMachine};

impl LocalMachine {
    /// Replace or clear the push target under mutation admission.
    ///
    /// # Errors
    /// Returns [`Error::InvalidPushTarget`] for a URL the observe process must not
    /// send to, admission failures, [`Error::RecordOwner`] when the record owner
    /// has stopped, and [`Error::Store`] when the record cannot be written.
    pub async fn set_push_target(
        &self,
        request: SetPushTargetRequest,
    ) -> Result<PushTargetSet, Error> {
        let url = request.url.map(admit).transpose()?;
        let local = self.clone();
        self.finish_mutation(async move {
            local
                .owner
                .mutate(move |store| {
                    store.set_push_target(url.clone())?;
                    Ok(PushTargetSet { url })
                })
                .await?
        })
        .await
    }
}

/// Pushes carry a signature, not a secret, but plain http still exposes crash
/// details on the wire. It is admitted only for hosts that never leave the
/// operator's network, which is how a local Cloud reaches lab Machines.
fn admit(url: Url) -> Result<Url, Error> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::InvalidPushTarget("must not carry credentials"));
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if url.host().is_some_and(|host| stays_local(&host)) => Ok(url),
        "http" => Err(Error::InvalidPushTarget(
            "must use https unless its host is loopback or private",
        )),
        _ => Err(Error::InvalidPushTarget("must use https")),
    }
}

fn stays_local(host: &Host<&str>) -> bool {
    let ip = match host {
        Host::Domain(domain) => return domain.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => IpAddr::V4(*ip),
        Host::Ipv6(ip) => IpAddr::V6(*ip),
    };
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{LocalMachineStore, RecordOwner};

    fn local(dir: &std::path::Path) -> LocalMachine {
        LocalMachine::new(RecordOwner::spawn(LocalMachineStore::open(dir).unwrap()).unwrap())
    }

    fn request(url: Option<&str>) -> SetPushTargetRequest {
        SetPushTargetRequest {
            url: url.map(|url| url.parse().unwrap()),
        }
    }

    fn persisted(dir: &std::path::Path) -> Option<String> {
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("machine.json")).unwrap()).unwrap();
        record
            .get("push_target")
            .map(|target| target.as_str().unwrap().to_owned())
    }

    #[tokio::test]
    async fn set_persists_the_target_and_clear_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = "https://cloud.example/api/machines/aa/observe";

        let set = local(dir.path())
            .set_push_target(request(Some(target)))
            .await
            .unwrap();
        assert_eq!(set.url.as_ref().map(Url::as_str), Some(target));
        assert_eq!(persisted(dir.path()).as_deref(), Some(target));
        let reopened = LocalMachineStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.record().push_target().map(Url::as_str),
            Some(target)
        );
        drop(reopened);

        let cleared = local(dir.path())
            .set_push_target(request(None))
            .await
            .unwrap();
        assert_eq!(cleared.url, None);
        assert_eq!(persisted(dir.path()), None);
    }

    #[tokio::test]
    async fn a_bad_url_is_refused_and_keeps_the_current_target() {
        let dir = tempfile::tempdir().unwrap();
        let machine = local(dir.path());
        let target = "https://cloud.example/observe";
        machine
            .set_push_target(request(Some(target)))
            .await
            .unwrap();

        for bad in [
            "http://cloud.example/observe",
            "ftp://cloud.example/observe",
            "https://user:pass@cloud.example/observe",
            "http://8.8.8.8/observe",
        ] {
            let error = machine
                .set_push_target(request(Some(bad)))
                .await
                .unwrap_err();
            assert!(
                matches!(error, Error::InvalidPushTarget(_)),
                "{bad}: {error}"
            );
        }
        assert_eq!(persisted(dir.path()).as_deref(), Some(target));
    }

    #[test]
    fn plain_http_is_admitted_only_for_hosts_inside_the_operator_network() {
        for local in [
            "http://localhost:3000/observe",
            "http://127.0.0.1:3000/observe",
            "http://[::1]:3000/observe",
            "http://10.0.0.5/observe",
            "http://192.168.122.1:3000/observe",
            "http://[fd00::1]/observe",
        ] {
            assert!(admit(local.parse().unwrap()).is_ok(), "{local}");
        }
        assert!(admit("http://cloud.example/observe".parse().unwrap()).is_err());
    }
}
