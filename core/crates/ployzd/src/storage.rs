//! Access to the local volume plugin's storage admission boundary.

use std::path::PathBuf;

use ployz_core::{RpcError, StorageCapacityError};
use serde::{Serialize, de::DeserializeOwned};

/// The socket-activated volume plugin, reached over its Docker plugin socket.
#[derive(Clone, Debug)]
pub(crate) struct Plugin {
    socket: PathBuf,
}

impl Default for Plugin {
    fn default() -> Self {
        Self::at(crate::VOLUME_PLUGIN_SOCKET.into())
    }
}

impl Plugin {
    pub(crate) fn at(socket: PathBuf) -> Self {
        Self { socket }
    }

    /// Ask the plugin for a bounded storage operation.
    pub(crate) async fn call<T: DeserializeOwned>(
        &self,
        route: &str,
        request: &impl Serialize,
    ) -> Result<T, RpcError> {
        async {
            reqwest::Client::builder()
                .unix_socket(self.socket.clone())
                .timeout(ployz_core::VOLUME_PLUGIN_CALL_TIMEOUT)
                .build()?
                .post(format!("http://localhost/{route}"))
                .json(request)
                .send()
                .await?
                .error_for_status()?
                .json::<Result<T, RpcError>>()
                .await
        }
        .await
        .map_err(|error: reqwest::Error| {
            StorageCapacityError::StorageCapacityUnknown {
                message: ployz_core::error_chain::inline(&error),
            }
            .into_rpc_error()
        })?
    }
}

/// How often the daemon asks the plugin to sweep the Trash. The plugin counts at most one
/// hour per hour of wall time, so a tick that comes early counts nothing.
const TRASH_SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(3600);

/// Asks the plugin for one Trash sweep at start and every hour after, until `shutdown`.
/// A Machine without a Pool sweeps nothing; a sweep that fails is logged and tried
/// again at the next tick.
pub(crate) async fn sweep_trash_hourly(
    plugin: Plugin,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let mut ticks = tokio::time::interval(TRASH_SWEEP_EVERY);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = ticks.tick() => {}
        }
        match plugin
            .call::<crate::volume_plugin::TrashSwept>(crate::volume_copy::TRASH_SWEEP_ROUTE, &())
            .await
        {
            Ok(swept) => {
                for id in &swept.destroyed {
                    tracing::info!(%id, hours = swept.hours, "the Trash sweep destroyed an expired entry");
                }
                for failure in &swept.failed {
                    tracing::warn!(id = %failure.id, reason = %failure.reason, "the Trash sweep could not settle an entry; it tries again next hour");
                }
            }
            Err(error) => {
                tracing::warn!(error = %ployz_core::error_chain::inline(&error), "the Trash sweep did not run; it tries again next hour");
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        collections::BTreeMap,
        path::Path,
        sync::{Arc, Mutex},
    };

    use axum::{Router, extract::State, http::Uri, response::IntoResponse};
    use serde_json::Value;

    use super::Plugin;

    type Effect = Arc<dyn Fn() + Send + Sync>;

    /// A plugin that answers each route with a canned reply and records every call. A
    /// route without a reply answers an internal error.
    #[derive(Clone, Default)]
    pub(crate) struct FakePlugin {
        pub(crate) calls: Arc<Mutex<Vec<(String, Value)>>>,
        pub(crate) replies: Arc<Mutex<BTreeMap<String, Value>>>,
        effects: Arc<Mutex<BTreeMap<String, Effect>>>,
    }

    impl FakePlugin {
        pub(crate) fn reply(&self, route: &str, reply: Value) {
            self.replies.lock().unwrap().insert(route.to_owned(), reply);
        }

        /// Runs `effect` each time `route` is called, before the canned reply.
        pub(crate) fn on(&self, route: &str, effect: impl Fn() + Send + Sync + 'static) {
            self.effects
                .lock()
                .unwrap()
                .insert(route.to_owned(), Arc::new(effect));
        }

        pub(crate) fn routes_called(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|(route, _)| route.clone())
                .collect()
        }

        /// Serves this fake on a socket under `directory`.
        pub(crate) fn serve(&self, directory: &Path) -> (Plugin, tokio::task::JoinHandle<()>) {
            let socket = directory.join("plugin.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let router = Router::new()
                .fallback(axum::routing::post(answer))
                .with_state(self.clone());
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            (Plugin::at(socket), server)
        }
    }

    async fn answer(
        State(fake): State<FakePlugin>,
        uri: Uri,
        axum::Json(request): axum::Json<Value>,
    ) -> axum::response::Response {
        let route = uri.path().trim_start_matches('/').to_owned();
        fake.calls.lock().unwrap().push((route.clone(), request));
        let effect = fake.effects.lock().unwrap().get(&route).cloned();
        if let Some(effect) = effect {
            effect();
        }
        let reply = fake.replies.lock().unwrap().get(&route).cloned();
        match reply {
            Some(reply) => axum::Json(reply).into_response(),
            None => axum::Json(serde_json::json!({"Err": {
                "code": "internal",
                "message": format!("fake plugin has no reply for {route}"),
                "details": null,
            }}))
            .into_response(),
        }
    }
}
