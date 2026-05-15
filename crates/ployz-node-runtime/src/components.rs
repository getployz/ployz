use ployz_runtime_api::{NoopRuntimeHandle, RuntimeHandle};
use tracing::warn;

pub struct RuntimeComponents {
    nats_control: Box<dyn RuntimeHandle>,
    zfs_transfer: Box<dyn RuntimeHandle>,
    image_receiver: Box<dyn RuntimeHandle>,
    gateway: Box<dyn RuntimeHandle>,
    dns: Box<dyn RuntimeHandle>,
}

impl Default for RuntimeComponents {
    fn default() -> Self {
        Self::noop()
    }
}

impl RuntimeComponents {
    #[must_use]
    pub fn noop() -> Self {
        Self {
            nats_control: Box::new(NoopRuntimeHandle),
            zfs_transfer: Box::new(NoopRuntimeHandle),
            image_receiver: Box::new(NoopRuntimeHandle),
            gateway: Box::new(NoopRuntimeHandle),
            dns: Box::new(NoopRuntimeHandle),
        }
    }

    pub fn set_nats_control(&mut self, handle: Box<dyn RuntimeHandle>) {
        self.nats_control = handle;
    }

    pub fn set_zfs_transfer(&mut self, handle: Box<dyn RuntimeHandle>) {
        self.zfs_transfer = handle;
    }

    pub fn set_image_receiver(&mut self, handle: Box<dyn RuntimeHandle>) {
        self.image_receiver = handle;
    }

    pub fn set_gateway(&mut self, handle: Box<dyn RuntimeHandle>) {
        self.gateway = handle;
    }

    pub fn set_dns(&mut self, handle: Box<dyn RuntimeHandle>) {
        self.dns = handle;
    }

    pub fn replace_dns(&mut self, handle: Box<dyn RuntimeHandle>) -> Box<dyn RuntimeHandle> {
        std::mem::replace(&mut self.dns, handle)
    }

    pub fn replace_gateway(&mut self, handle: Box<dyn RuntimeHandle>) -> Box<dyn RuntimeHandle> {
        std::mem::replace(&mut self.gateway, handle)
    }

    pub fn replace_image_receiver(
        &mut self,
        handle: Box<dyn RuntimeHandle>,
    ) -> Box<dyn RuntimeHandle> {
        std::mem::replace(&mut self.image_receiver, handle)
    }

    pub fn take_nats_control(&mut self) -> Box<dyn RuntimeHandle> {
        std::mem::replace(&mut self.nats_control, Box::new(NoopRuntimeHandle))
    }

    pub async fn shutdown_nats_control(&mut self) {
        let nats_control = self.take_nats_control();
        let _ = nats_control.shutdown().await;
    }

    pub async fn shutdown_edge_and_image_receiver(
        &mut self,
        context: &'static str,
    ) -> Option<String> {
        let dns = self.replace_dns(Box::new(NoopRuntimeHandle));
        if let Err(error) = dns.shutdown().await {
            warn!(?error, context, "dns stop failed");
        }

        let gateway = self.replace_gateway(Box::new(NoopRuntimeHandle));
        let gateway_error = gateway.shutdown().await.err();

        let image_receiver = self.replace_image_receiver(Box::new(NoopRuntimeHandle));
        if let Err(error) = image_receiver.shutdown().await {
            warn!(?error, context, "image receiver stop failed");
        }

        gateway_error
    }

    pub async fn shutdown_zfs_transfer(&mut self, context: &'static str) {
        let zfs_transfer = std::mem::replace(&mut self.zfs_transfer, Box::new(NoopRuntimeHandle));
        if let Err(error) = zfs_transfer.shutdown().await {
            warn!(?error, context, "zfs transfer listener stop failed");
        }
    }

    pub async fn rollback_startup(&mut self) {
        let dns = self.replace_dns(Box::new(NoopRuntimeHandle));
        if let Err(error) = dns.shutdown().await {
            warn!(?error, "dns rollback failed");
        }

        let gateway = self.replace_gateway(Box::new(NoopRuntimeHandle));
        if let Err(error) = gateway.shutdown().await {
            warn!(?error, "gateway rollback failed");
        }

        let nats_control = std::mem::replace(&mut self.nats_control, Box::new(NoopRuntimeHandle));
        let _ = nats_control.shutdown().await;

        let zfs_transfer = std::mem::replace(&mut self.zfs_transfer, Box::new(NoopRuntimeHandle));
        let _ = zfs_transfer.shutdown().await;

        let image_receiver =
            std::mem::replace(&mut self.image_receiver, Box::new(NoopRuntimeHandle));
        let _ = image_receiver.shutdown().await;
    }

    pub async fn shutdown_for_daemon_stop(self) {
        let _ = self.dns.detach().await;
        let _ = self.gateway.detach().await;
        let _ = self.image_receiver.shutdown().await;
        let _ = self.zfs_transfer.shutdown().await;
        let _ = self.nats_control.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn startup_rollback_shuts_down_started_components_in_dependency_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut components = RuntimeComponents::noop();
        components.set_nats_control(recording_handle("nats_shutdown", events.clone()));
        components.set_zfs_transfer(recording_handle("zfs_shutdown", events.clone()));
        components.set_image_receiver(recording_handle("image_shutdown", events.clone()));
        components.set_gateway(recording_handle("gateway_shutdown", events.clone()));
        components.set_dns(recording_handle("dns_shutdown", events.clone()));

        components.rollback_startup().await;

        assert_eq!(
            recorded(&events),
            vec![
                "dns_shutdown",
                "gateway_shutdown",
                "nats_shutdown",
                "zfs_shutdown",
                "image_shutdown"
            ]
        );
    }

    #[tokio::test]
    async fn daemon_stop_detaches_edge_and_stops_control_components_in_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut components = RuntimeComponents::noop();
        components.set_nats_control(recording_handle("nats_shutdown", events.clone()));
        components.set_zfs_transfer(recording_handle("zfs_shutdown", events.clone()));
        components.set_image_receiver(recording_handle("image_shutdown", events.clone()));
        components.set_gateway(recording_handle("gateway", events.clone()));
        components.set_dns(recording_handle("dns", events.clone()));

        components.shutdown_for_daemon_stop().await;

        assert_eq!(
            recorded(&events),
            vec![
                "dns_detach",
                "gateway_detach",
                "image_shutdown",
                "zfs_shutdown",
                "nats_shutdown"
            ]
        );
    }

    fn recording_handle(
        name: &'static str,
        events: Arc<Mutex<Vec<&'static str>>>,
    ) -> Box<dyn RuntimeHandle> {
        Box::new(RecordingHandle { name, events })
    }

    fn recorded(events: &Arc<Mutex<Vec<&'static str>>>) -> Vec<&'static str> {
        events.lock().expect("events").clone()
    }

    struct RecordingHandle {
        name: &'static str,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl RuntimeHandle for RecordingHandle {
        async fn shutdown(self: Box<Self>) -> Result<(), String> {
            self.events.lock().expect("events").push(self.name);
            Ok(())
        }

        async fn detach(self: Box<Self>) -> Result<(), String> {
            self.events.lock().expect("events").push(match self.name {
                "dns" => "dns_detach",
                "gateway" => "gateway_detach",
                other => other,
            });
            Ok(())
        }
    }
}
