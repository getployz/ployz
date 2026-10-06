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
        Self::at("/run/docker/plugins/ployz.sock".into())
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
                .timeout(std::time::Duration::from_secs(120))
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
                message: error.to_string(),
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
        sync::{Arc, Mutex},
    };

    use axum::{Router, extract::State, http::Uri, response::IntoResponse};
    use serde_json::Value;

    use super::Plugin;

    /// A plugin that answers each route with a canned reply and records every call. A
    /// route without a reply answers an internal error.
    #[derive(Clone, Default)]
    pub(crate) struct FakePlugin {
        pub(crate) calls: Arc<Mutex<Vec<(String, Value)>>>,
        pub(crate) replies: Arc<Mutex<BTreeMap<String, Value>>>,
    }

    impl FakePlugin {
        pub(crate) fn reply(&self, route: &str, reply: Value) {
            self.replies.lock().unwrap().insert(route.to_owned(), reply);
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
