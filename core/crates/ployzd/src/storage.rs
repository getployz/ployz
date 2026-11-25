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

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        collections::BTreeMap,
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use axum::{Router, extract::State, http::Uri, response::IntoResponse};
    use serde_json::Value;

    use super::Plugin;
    use crate::volume_copy::{INSPECT_ROUTE, WRITER_ROUTE};

    type Effect = Arc<dyn Fn() + Send + Sync>;

    /// A plugin that answers each route with a canned reply and records every call. A
    /// route without a reply answers an internal error. Like the plugin, its Writer
    /// route records the request's start image on the root it answers, and Inspect
    /// answers it from then on.
    #[derive(Clone, Default)]
    pub(crate) struct FakePlugin {
        pub(crate) calls: Arc<Mutex<Vec<(String, Value)>>>,
        pub(crate) replies: Arc<Mutex<BTreeMap<String, Value>>>,
        effects: Arc<Mutex<BTreeMap<String, Effect>>>,
        gates: Arc<Mutex<BTreeMap<String, Arc<tokio::sync::RwLock<()>>>>>,
        records_no_image: Arc<AtomicBool>,
    }

    impl FakePlugin {
        pub(crate) fn reply(&self, route: &str, reply: Value) {
            self.replies.lock().unwrap().insert(route.to_owned(), reply);
        }

        /// Answers the Writer route without the start image, as a plugin from before
        /// `ployz:image` does.
        pub(crate) fn records_no_image(&self) {
            self.records_no_image.store(true, Ordering::SeqCst);
        }

        /// Runs `effect` each time `route` is called, before the canned reply.
        pub(crate) fn on(&self, route: &str, effect: impl Fn() + Send + Sync + 'static) {
            self.effects
                .lock()
                .unwrap()
                .insert(route.to_owned(), Arc::new(effect));
        }

        /// Holds every call to `route` after it is recorded, until the guard drops.
        pub(crate) async fn hold(&self, route: &str) -> tokio::sync::OwnedRwLockWriteGuard<()> {
            let gate = Arc::clone(
                self.gates
                    .lock()
                    .unwrap()
                    .entry(route.to_owned())
                    .or_default(),
            );
            gate.write_owned().await
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

    impl FakePlugin {
        /// Sets `placed` on the root the Writer route and Inspect answer, or clears it.
        fn record_image(&self, image: Option<&Value>) {
            let mut replies = self.replies.lock().unwrap();
            for route in [WRITER_ROUTE, INSPECT_ROUTE] {
                let Some(root) = replies
                    .get_mut(route)
                    .and_then(|reply| reply.get_mut("Ok"))
                    .and_then(Value::as_object_mut)
                    .filter(|root| root.get("kind").and_then(Value::as_str) == Some("root"))
                else {
                    continue;
                };
                match image {
                    Some(image) => root.insert("placed".to_owned(), image.clone()),
                    None => root.remove("placed"),
                };
            }
        }
    }

    async fn answer(
        State(fake): State<FakePlugin>,
        uri: Uri,
        axum::Json(request): axum::Json<Value>,
    ) -> axum::response::Response {
        let route = uri.path().trim_start_matches('/').to_owned();
        fake.calls
            .lock()
            .unwrap()
            .push((route.clone(), request.clone()));
        let gate = fake.gates.lock().unwrap().get(&route).cloned();
        if let Some(gate) = gate {
            drop(gate.read().await);
        }
        let effect = fake.effects.lock().unwrap().get(&route).cloned();
        if let Some(effect) = effect {
            effect();
        }
        if route == WRITER_ROUTE && !fake.records_no_image.load(Ordering::SeqCst) {
            fake.record_image(request.get("image"));
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
