//! Stateful fake-Docker support for Volume tests.

use ployz_core::Namespace;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use bollard::Docker;
use futures_util::StreamExt as _;

use super::*;
use ployz_core::{Machine, MachineGateway, MachineId, ResolvedServiceSpec, VolumeSource};

pub(crate) type StopHook = Box<dyn Fn(&str) + Send>;

#[derive(Clone, Default)]
pub(crate) struct FakeDocker {
    pub(crate) named_containers: Option<Arc<Mutex<BTreeMap<String, serde_json::Value>>>>,
    pub(crate) requests: Arc<Mutex<Vec<(Method, String)>>>,
    pub(crate) request_bodies: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    pub(crate) volumes: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
    pub(crate) fail_after_create: Arc<Mutex<BTreeSet<String>>>,
    /// Volumes whose next Create fails after Docker recorded them without labels or options,
    /// as when the plugin dies inside Create and a later Get hands Docker the root.
    pub(crate) crash_inside_create: Arc<Mutex<BTreeSet<String>>>,
    /// Volume names and Container IDs whose next inspect answers a Docker 500.
    pub(crate) fail_inspect_once: Arc<Mutex<BTreeSet<String>>>,
    /// Named mode: Service names whose next Container to start answers this many
    /// inspects once it runs, then fails the next with a Docker 500.
    pub(crate) fail_inspect_after_start: Arc<Mutex<BTreeMap<String, usize>>>,
    /// The Container IDs a start armed from `fail_inspect_after_start`, with the
    /// inspects each answers before the failing one.
    pub(crate) failing_inspects: Arc<Mutex<BTreeMap<String, usize>>>,
    pub(crate) existing_container: Arc<Mutex<Option<serde_json::Value>>>,
    /// Docker's `info` reports the containerd image store, which a digest pull needs.
    pub(crate) containerd_store: bool,
    pub(crate) volume_users: Arc<Mutex<BTreeMap<String, Vec<serde_json::Value>>>>,
    pub(crate) image_barrier: Option<Arc<tokio::sync::Barrier>>,
    pub(crate) create_barrier: Option<Arc<tokio::sync::Barrier>>,
    /// A start waits here twice: once to announce it began, once to be released.
    pub(crate) start_barrier: Option<Arc<tokio::sync::Barrier>>,
    pub(crate) inspect_hold: Arc<Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    /// The next inspect reads the Container first, then its answer waits here twice.
    pub(crate) answered_hold: Arc<Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    /// The next Container list is answered, then its reply waits here twice.
    pub(crate) listed_hold: Arc<Mutex<Option<Arc<tokio::sync::Barrier>>>>,
    pub(crate) reject_list: Arc<AtomicBool>,
    /// Every Volume create answers a Docker 500 with this message, as Docker relays a
    /// volume plugin's refusal.
    pub(crate) refuse_volume_create: Arc<Mutex<Option<String>>>,
    /// Named mode: every stop fails and leaves the Container as it was.
    pub(crate) refuse_stop: Arc<AtomicBool>,
    /// Named mode: a stop lands and the next inspect sees it, then a restart policy
    /// brings the Container back for every inspect after that.
    pub(crate) restart_after_stop: Arc<AtomicBool>,
    /// Named mode: every start lands, then the process exits at once.
    pub(crate) exit_on_start: Arc<AtomicBool>,
    /// Named mode: Docker refuses to create a Container of these Service names.
    pub(crate) refuse_create_of: Arc<Mutex<BTreeSet<String>>>,
    /// Named mode: Docker refuses to remove a Container of these Service names.
    pub(crate) refuse_remove_of: Arc<Mutex<BTreeSet<String>>>,
    /// Named mode: each removal's Container ID, and whether it took the Container's
    /// anonymous volumes.
    pub(crate) removals: Arc<Mutex<Vec<(String, bool)>>>,
    /// Named mode: where every new Container gets a fresh anonymous volume, as an image's
    /// `VOLUME` declares one. A removal with `v=true` takes them; Docker marks each.
    pub(crate) anonymous_mounts: Arc<Mutex<Vec<String>>>,
    pub(crate) stopped_once: Arc<Mutex<BTreeSet<String>>>,
    /// Named mode: each start of a stopped Container stamps a new `StartedAt`, and a
    /// stop keeps it and stamps `FinishedAt`, as Docker does.
    pub(crate) stamp_starts: bool,
    /// Named mode: the code a stopped process exits with, as an app that exits non-zero
    /// on SIGTERM leaves it.
    pub(crate) stop_exit_code: i64,
    /// Every stop waits while a test holds this for writing; the request is recorded first.
    pub(crate) stop_gate: Arc<tokio::sync::RwLock<()>>,
    /// Every inspect of this Container waits while a test holds `inspect_gate` for writing.
    pub(crate) gated_inspects: Arc<Mutex<Option<String>>>,
    pub(crate) inspect_gate: Arc<tokio::sync::RwLock<()>>,
    pub(crate) on_stop: Arc<Mutex<Option<StopHook>>>,
    pub(crate) list_warnings: Arc<Mutex<Vec<String>>>,
    /// Docker Volumes a list answers beside its fixed ones.
    pub(crate) listed_volumes: Arc<Mutex<Vec<serde_json::Value>>>,
    /// Another start attaches the network first, so this connect finds it attached.
    pub(crate) connect_raced: Arc<AtomicBool>,
    /// Named mode: a list answers with the named Containers its label filters match,
    /// as an observation publisher needs.
    pub(crate) list_named: bool,
    /// Named mode: once an inspect answers a Container Docker OOM-killed, every later
    /// inspect sends its headers and then stalls inside its body.
    pub(crate) stall_after_oom: Arc<AtomicBool>,
    pub(crate) oom_answered: Arc<AtomicBool>,
    /// Named mode: every inspect of a running Container sends its headers, then its
    /// body this long after.
    pub(crate) slow_running_inspects: Arc<Mutex<Duration>>,
    /// The multiplexed body every Container log request answers with.
    pub(crate) logs: Arc<Mutex<Vec<u8>>>,
    /// What a network inspect answers in place of an empty Ployz bridge.
    pub(crate) network: Arc<Mutex<Option<serde_json::Value>>>,
    /// The images this Docker holds by reference: `Some(id)` is the image a tag names,
    /// `None` one it does not hold. A reference listed nowhere answers an ID of its own:
    /// the digest or image ID it names, or one made from its name. A digest never
    /// follows a tag.
    pub(crate) images: Arc<Mutex<BTreeMap<String, Option<String>>>>,
    /// What the registry serves for each tag, by ID. A tag listed nowhere serves the ID
    /// made from its name. A pull of a tag writes it into [`Self::images`].
    pub(crate) registry: Arc<Mutex<BTreeMap<String, String>>>,
    /// What an image listing answers before its `dangling` and `label` filters: Docker's
    /// summaries, each with the `LastTagTime` an inspect of its ID answers.
    pub(crate) listed_images: Arc<Mutex<Vec<serde_json::Value>>>,
    /// Each image a remove named, in order.
    pub(crate) removed_images: Arc<Mutex<Vec<String>>>,
    /// When set, each image pull spends one; once none are left, a pull answers the
    /// registry's rate limit.
    pub(crate) pulls_left: Arc<Mutex<Option<usize>>>,
    /// Images the registry serves only to a pull that sends a login.
    pub(crate) private: Arc<Mutex<BTreeSet<String>>>,
    /// The registry mirrors `/info` reports Docker is configured with.
    pub(crate) mirrors: Arc<Mutex<Vec<String>>>,
}

impl FakeDocker {
    fn image(&self, reference: &str) -> Option<String> {
        match self.images.lock().unwrap().get(reference) {
            Some(held) => held.clone(),
            None => Some(image_id(reference)),
        }
    }

    fn served(&self, tag: &str) -> String {
        self.registry
            .lock()
            .unwrap()
            .get(tag)
            .cloned()
            .unwrap_or_else(|| image_id(tag))
    }
}

async fn fake_docker(
    State(fake): State<FakeDocker>,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    if method == Method::POST
        && uri.path().ends_with("/images/create")
        && !headers.contains_key("x-registry-auth")
        && uri.query().is_some_and(|query| {
            let query = query.replace("%2F", "/").replace("%3A", ":");
            fake.private
                .lock()
                .unwrap()
                .iter()
                .any(|image| query.contains(image.as_str()))
        })
    {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(
                serde_json::json!({"message": "pull access denied: authentication required"}),
            ),
        )
            .into_response();
    }
    let hold = (method == Method::GET && is_inspect(uri.path()))
        .then(|| fake.answered_hold.lock().unwrap().take())
        .flatten()
        .or_else(|| {
            (method == Method::GET && uri.path().ends_with("/containers/json"))
                .then(|| fake.listed_hold.lock().unwrap().take())
                .flatten()
        });
    let response = answer(fake, method, uri, body).await;
    if let Some(hold) = hold {
        hold.wait().await;
        hold.wait().await;
    }
    response
}

fn slow_body(delay: Duration, body: serde_json::Value) -> Response {
    let body = futures_util::stream::once(async move {
        // real-wait: outlasts the daemon's real-clock inspect deadline across the fake's TCP socket.
        tokio::time::sleep(delay).await;
        Ok::<_, std::io::Error>(Bytes::from(body.to_string()))
    });
    axum::body::Body::from_stream(body).into_response()
}

/// A 200 whose body starts and never finishes.
fn stalled_body() -> Response {
    let body = futures_util::stream::once(async { Ok(Bytes::from_static(b"{")) })
        .chain(futures_util::stream::pending::<Result<Bytes, std::io::Error>>());
    axum::body::Body::from_stream(body).into_response()
}

fn is_inspect(path: &str) -> bool {
    path.contains("/containers/") && path.ends_with("/json") && !path.ends_with("/containers/json")
}

async fn answer(fake: FakeDocker, method: Method, uri: Uri, body: Bytes) -> Response {
    let path = uri.path().to_owned();
    fake.requests
        .lock()
        .unwrap()
        .push((method.clone(), path.clone()));
    if !body.is_empty() {
        fake.request_bodies.lock().unwrap().push((
            path.clone(),
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        ));
    }
    if method == Method::POST && path.ends_with("/stop") {
        drop(fake.stop_gate.read().await);
    }
    if method == Method::GET && path == "/info" {
        let driver = if fake.containerd_store {
            "io.containerd.snapshotter.v1"
        } else {
            "overlay2"
        };
        return axum::Json(serde_json::json!({
            "DriverStatus": [["driver-type", driver]],
            "RegistryConfig": {"Mirrors": *fake.mirrors.lock().unwrap()},
        }))
        .into_response();
    }
    if method == Method::GET && path.contains("/containers/") && path.ends_with("/logs") {
        return fake.logs.lock().unwrap().clone().into_response();
    }
    if method == Method::GET && path.ends_with("/events") {
        return axum::body::Body::from_stream(futures_util::stream::pending::<
            Result<Bytes, std::io::Error>,
        >())
        .into_response();
    }
    let gated = method == Method::GET
        && is_inspect(&path)
        && fake
            .gated_inspects
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|container| path.contains(container.as_str()));
    if gated {
        drop(fake.inspect_gate.read().await);
    }
    let hold = (method == Method::GET && is_inspect(&path))
        .then(|| fake.inspect_hold.lock().unwrap().take())
        .flatten();
    if let Some(hold) = hold {
        hold.wait().await;
        hold.wait().await;
    }
    if let Some(containers) = &fake.named_containers {
        if method == Method::POST
            && path.ends_with("/start")
            && let Some(barrier) = &fake.start_barrier
        {
            barrier.wait().await;
            barrier.wait().await;
        }
        let mut containers = containers.lock().unwrap();
        if fake.list_named && method == Method::GET && path.ends_with("/containers/json") {
            return Json(listed_by_labels(&containers, uri.query())).into_response();
        }
        if method == Method::POST && path.ends_with("/containers/create") {
            let name = uri
                .query()
                .unwrap()
                .split('&')
                .find_map(|part| part.strip_prefix("name="))
                .unwrap();
            if containers.contains_key(name) {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({"message":"name in use"})),
                )
                    .into_response();
            }
            let id = format!("{}{}", MachineId::random(), MachineId::random());
            let config: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if config
                .pointer("/Labels/ployz.service.name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|service| fake.refuse_create_of.lock().unwrap().contains(service))
            {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"message":"cannot create container"})),
                )
                    .into_response();
            }
            // Docker answers the host config beside the config, not inside it.
            let host_config = config.get("HostConfig").cloned().unwrap_or_default();
            let image = config
                .get("Image")
                .and_then(serde_json::Value::as_str)
                .and_then(|image| fake.image(image));
            // Docker answers each mount by the Volume's name and where it is mounted.
            let mut mounts: Vec<serde_json::Value> = host_config
                .get("Mounts")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .map(|mount| {
                    serde_json::json!({
                        "Type": mount.get("Type"),
                        "Name": mount.get("Source"),
                        "Destination": mount.get("Target"),
                        "Driver": mount.pointer("/VolumeOptions/DriverConfig/Name"),
                    })
                })
                .collect();
            for destination in fake.anonymous_mounts.lock().unwrap().iter() {
                let volume = format!("{}{}", MachineId::random(), MachineId::random());
                fake.volumes.lock().unwrap().insert(
                    volume.clone(),
                    serde_json::json!({
                        "Name": volume,
                        "Driver": "local",
                        "Mountpoint": format!("/var/lib/docker/volumes/{volume}/_data"),
                        "Labels": {ANONYMOUS_VOLUME_LABEL: ""},
                        "CreatedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        "Scope": "local",
                        "Options": null,
                    }),
                );
                mounts.push(serde_json::json!({
                    "Type": "volume",
                    "Name": volume,
                    "Destination": destination,
                    "Driver": "local",
                }));
            }
            containers.insert(name.into(), serde_json::json!({"Id": id, "Name": format!("/{name}"), "Image": image, "Config": config, "HostConfig": host_config, "Mounts": mounts, "State":{"Status":"created"}}));
            return (
                StatusCode::CREATED,
                Json(serde_json::json!({"Id":id,"Warnings":[]})),
            )
                .into_response();
        }
        if path.contains("/containers/") && !path.ends_with("/containers/json") {
            let selector = path
                .split("/containers/")
                .nth(1)
                .unwrap()
                .split('/')
                .next()
                .unwrap();
            let found = containers
                .iter()
                .find(|(name, container)| name.as_str() == selector || container["Id"] == selector)
                .map(|(name, container)| (name.clone(), container.clone()));
            let Some((name, container)) = found else {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"message":"no such container"})),
                )
                    .into_response();
            };
            if method == Method::GET {
                if container
                    .get("Id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| fake.fail_inspect_once.lock().unwrap().remove(id))
                {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"message":"detail temporarily unavailable"})),
                    )
                        .into_response();
                }
                if let Some(id) = container.get("Id").and_then(serde_json::Value::as_str) {
                    let mut failing = fake.failing_inspects.lock().unwrap();
                    match failing.get_mut(id) {
                        Some(0) => {
                            failing.remove(id);
                            return (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                Json(serde_json::json!({"message":"inspect temporarily failed"})),
                            )
                                .into_response();
                        }
                        Some(left) => *left -= 1,
                        None => {}
                    }
                }
                if fake.stopped_once.lock().unwrap().remove(&name) {
                    containers.get_mut(&name).unwrap()["State"] =
                        serde_json::json!({"Status": "running", "Running": true});
                }
                if fake.oom_answered.load(Ordering::SeqCst) {
                    return stalled_body();
                }
                if fake.stall_after_oom.load(Ordering::SeqCst)
                    && container.pointer("/State/OOMKilled") == Some(&serde_json::Value::Bool(true))
                {
                    fake.oom_answered.store(true, Ordering::SeqCst);
                }
                let delay = *fake.slow_running_inspects.lock().unwrap();
                if !delay.is_zero()
                    && container.pointer("/State/Status") == Some(&serde_json::json!("running"))
                {
                    return slow_body(delay, container);
                }
                return Json(container).into_response();
            }
            if method == Method::DELETE {
                if container
                    .pointer("/Config/Labels/ployz.service.name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|service| fake.refuse_remove_of.lock().unwrap().contains(service))
                {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"message":"cannot remove container"})),
                    )
                        .into_response();
                }
                let volumes = uri
                    .query()
                    .is_some_and(|query| query.split('&').any(|part| part == "v=true"));
                fake.removals.lock().unwrap().push((
                    container
                        .get("Id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    volumes,
                ));
                if volumes {
                    let mut held = fake.volumes.lock().unwrap();
                    for mounted in mounted_volumes(&container) {
                        if held
                            .get(&mounted)
                            .and_then(|volume| volume.pointer("/Labels"))
                            .is_some_and(|labels| labels.get(ANONYMOUS_VOLUME_LABEL).is_some())
                        {
                            held.remove(&mounted);
                        }
                    }
                }
                containers.remove(&name);
                return StatusCode::NO_CONTENT.into_response();
            }
            if method == Method::POST
                && path.ends_with("/stop")
                && let Some(hook) = fake.on_stop.lock().unwrap().as_ref()
            {
                hook(
                    container
                        .get("Id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap(),
                );
            }
            if method == Method::POST && (path.ends_with("/start") || path.ends_with("/stop")) {
                if fake.refuse_stop.load(Ordering::SeqCst) && path.ends_with("/stop") {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"message":"cannot stop container"})),
                    )
                        .into_response();
                }
                // Docker leaves a Container whose process already exited as it was, and
                // answers 304, which the client reads as success.
                if path.ends_with("/stop")
                    && container.pointer("/State/Status") != Some(&"running".into())
                {
                    return StatusCode::NOT_MODIFIED.into_response();
                }
                let status = if path.ends_with("/start") {
                    "running"
                } else {
                    if fake.restart_after_stop.load(Ordering::SeqCst) {
                        fake.stopped_once.lock().unwrap().insert(name.clone());
                    }
                    "exited"
                };
                let (status, code) =
                    if status == "running" && fake.exit_on_start.load(Ordering::SeqCst) {
                        ("exited", 1)
                    } else if status == "exited" {
                        (status, fake.stop_exit_code)
                    } else {
                        (status, 0)
                    };
                let container = containers.get_mut(&name).unwrap();
                let previous = container["State"].take();
                let mut state = serde_json::json!({"Status": status, "ExitCode": code});
                if fake.stamp_starts && (status == "running" || path.ends_with("/stop")) {
                    let started_at = if path.ends_with("/stop")
                        || previous.get("Status") == Some(&"running".into())
                    {
                        previous.get("StartedAt").cloned().unwrap_or_default()
                    } else {
                        chrono::Utc::now().to_rfc3339().into()
                    };
                    state
                        .as_object_mut()
                        .expect("a literal object")
                        .insert("StartedAt".into(), started_at);
                }
                if fake.stamp_starts && path.ends_with("/stop") {
                    state
                        .as_object_mut()
                        .expect("a literal object")
                        .insert("FinishedAt".into(), chrono::Utc::now().to_rfc3339().into());
                }
                if status == "running"
                    && let Some(answered) = container
                        .pointer("/Config/Labels/ployz.service.name")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|service| {
                            fake.fail_inspect_after_start
                                .lock()
                                .unwrap()
                                .remove(service)
                        })
                {
                    fake.failing_inspects
                        .lock()
                        .unwrap()
                        .insert(container["Id"].as_str().unwrap().to_owned(), answered);
                }
                container["State"] = state;
                return StatusCode::NO_CONTENT.into_response();
            }
            panic!("unexpected implicit lifecycle action: {method} {path}");
        }
    }
    let response = if method == Method::POST
        && path.ends_with("/containers/create")
        && (fake.create_barrier.is_some() || fake.image_barrier.is_some())
    {
        if let Some(barrier) = &fake.create_barrier {
            barrier.wait().await;
            barrier.wait().await;
        }
        let id = "a".repeat(64);
        let name = uri
            .query()
            .and_then(|query| query.split('&').find_map(|pair| pair.strip_prefix("name=")));
        *fake.existing_container.lock().unwrap() = Some(match name {
            Some(name) => serde_json::json!({"Id": id, "Name": format!("/{name}")}),
            None => serde_json::json!({"Id": id}),
        });
        (
            StatusCode::CREATED,
            serde_json::json!({"Id": id, "Warnings": []}),
        )
    } else if method == Method::GET && path.ends_with(&format!("/networks/{TUNNEL_NETWORK_NAME}")) {
        (
            StatusCode::OK,
            serde_json::json!({
                "Name": TUNNEL_NETWORK_NAME,
                "Driver": "bridge",
                "EnableIPv6": false,
                "Options": {"com.docker.network.bridge.name": TUNNEL_NETWORK_NAME},
            }),
        )
    } else if method == Method::GET && path.contains("/networks/") {
        let network = fake.network.lock().unwrap().clone().unwrap_or_else(|| {
            serde_json::json!({
                "Name": "ployz",
                "Labels": {labels::SYSTEM_MANAGED: ""},
                "IPAM": {"Config": [{"Subnet": "10.210.0.0/24", "Gateway": "10.210.0.1"}]},
            })
        });
        (StatusCode::OK, network)
    } else if method == Method::GET && path.ends_with("/containers/json") {
        let listed = uri.query().and_then(|query| {
            fake.volume_users
                .lock()
                .unwrap()
                .iter()
                .find_map(|(volume, holders)| {
                    (query.contains("volume") && query.contains(volume))
                        .then(|| serde_json::Value::Array(holders.clone()))
                })
        });
        let listed = listed.unwrap_or_else(|| {
            fake.existing_container
                .lock()
                .unwrap()
                .as_ref()
                .map(|container| {
                    serde_json::json!([{
                        "Id": container.get("Id").expect("fixture has ID"),
                        "Labels": container.pointer("/Config/Labels"),
                    }])
                })
                .unwrap_or_else(|| serde_json::json!([]))
        });
        (StatusCode::OK, listed)
    } else if method == Method::GET && path.contains("/containers/") && path.ends_with("/json") {
        let asked = path
            .trim_end_matches("/json")
            .rsplit('/')
            .next()
            .unwrap_or_default();
        // A container made under a name answers only to its ID or that name.
        fake.existing_container
            .lock()
            .unwrap()
            .clone()
            .filter(|container| {
                container
                    .get("Name")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|name| {
                        asked == name.trim_start_matches('/')
                            || container.get("Id").and_then(serde_json::Value::as_str)
                                == Some(asked)
                    })
            })
            .map_or_else(
                || {
                    (
                        StatusCode::NOT_FOUND,
                        serde_json::json!({"message":"no such container"}),
                    )
                },
                |container| (StatusCode::OK, container),
            )
    } else if method == Method::POST
        && path.ends_with("/images/create")
        && fake
            .pulls_left
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|left| left.checked_sub(1).map(|rest| *left = rest).is_none())
    {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"message":"toomanyrequests: You have reached your pull rate limit"}),
        )
    } else if method == Method::POST && path.ends_with("/images/create") {
        let pulled = uri
            .query()
            .and_then(|query| {
                query
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("fromImage="))
            })
            .map(|image| {
                image
                    .replace("%2F", "/")
                    .replace("%3A", ":")
                    .replace("%40", "@")
            })
            .unwrap_or_default();
        let id = if pulled.contains('@') {
            image_id(&pulled)
        } else {
            let id = fake.served(&pulled);
            fake.images.lock().unwrap().insert(pulled, Some(id.clone()));
            id
        };
        (
            StatusCode::OK,
            serde_json::json!({"status": format!("Digest: {id}")}),
        )
    } else if method == Method::GET && path.contains("/distribution/") {
        let tag = path
            .split("/distribution/")
            .nth(1)
            .unwrap_or_default()
            .trim_end_matches("/json");
        (
            StatusCode::OK,
            serde_json::json!({"Descriptor": {"digest": fake.served(tag)}, "Platforms": []}),
        )
    } else if method == Method::GET && path.ends_with("/images/json") {
        (
            StatusCode::OK,
            listed_images(&fake.listed_images.lock().unwrap(), uri.query()),
        )
    } else if method == Method::DELETE && path.contains("/images/") {
        let image = path.split("/images/").nth(1).unwrap_or_default().to_owned();
        fake.removed_images.lock().unwrap().push(image.clone());
        (StatusCode::OK, serde_json::json!([{"Deleted": image}]))
    } else if method == Method::GET && path.contains("/images/") && path.ends_with("/json") {
        if let Some(barrier) = &fake.image_barrier {
            barrier.wait().await;
            barrier.wait().await;
        }
        let image = path
            .split("/images/")
            .nth(1)
            .unwrap_or_default()
            .trim_end_matches("/json");
        let tagged = fake
            .listed_images
            .lock()
            .unwrap()
            .iter()
            .find(|listed| listed["Id"] == image)
            .map(|listed| listed["LastTagTime"].clone());
        match tagged {
            Some(time) => (
                StatusCode::OK,
                serde_json::json!({"Id": image, "Metadata": {"LastTagTime": time}}),
            ),
            None => match fake.image(image) {
                Some(id) => (StatusCode::OK, serde_json::json!({"Id": id})),
                None => (
                    StatusCode::NOT_FOUND,
                    serde_json::json!({"message": format!("No such image: {image}")}),
                ),
            },
        }
    } else if method == Method::GET
        && path.ends_with("/volumes")
        && fake.reject_list.load(Ordering::Relaxed)
    {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"message":"collection unavailable"}),
        )
    } else if method == Method::GET && path.ends_with("/volumes") {
        let mut volumes = vec![
            serde_json::json!({"Name":"plain","Driver":"local","Mountpoint":"/var/lib/docker/volumes/plain/_data"}),
            serde_json::json!({"Name":"healthy","Driver":"ployz","Mountpoint":"/var/lib/ployz-volumes/healthy"}),
            serde_json::json!({"Name":"malformed","Driver":"ployz","Mountpoint":"/var/lib/ployz-volumes/malformed"}),
            serde_json::json!({"Name":"unavailable","Driver":"ployz","Mountpoint":"/var/lib/ployz-volumes/unavailable"}),
            serde_json::json!({"Name":"mismatched","Driver":"ployz","Mountpoint":"/var/lib/ployz-volumes/mismatched"}),
        ];
        volumes.extend(fake.listed_volumes.lock().unwrap().iter().cloned());
        (
            StatusCode::OK,
            serde_json::json!({"Volumes":volumes,"Warnings":*fake.list_warnings.lock().unwrap()}),
        )
    } else if method == Method::GET && path.contains("/volumes/") {
        let name = path.rsplit('/').next().unwrap();
        if fake.fail_inspect_once.lock().unwrap().remove(name) {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"message":"detail temporarily unavailable"}),
            )
        } else if let Some(volume) = fake.volumes.lock().unwrap().get(name).cloned() {
            (StatusCode::OK, volume)
        } else if ["healthy", "data", "cache"].contains(&name) {
            (
                StatusCode::OK,
                serde_json::json!({
                    "Name":name,
                    "Driver":"ployz",
                    "Mountpoint":format!("/var/lib/ployz-volumes/{name}"),
                    "Status":{"bound_bytes":1073741824,"used_bytes":4096}
                }),
            )
        } else if name == "malformed" {
            (
                StatusCode::OK,
                serde_json::json!({
                    "Name":"malformed",
                    "Driver":"ployz",
                    "Mountpoint":"/var/lib/ployz-volumes/malformed",
                    "Status":{"bound_bytes":"not-a-number","used_bytes":4096}
                }),
            )
        } else if name == "unavailable" {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"message":"detail unavailable"}),
            )
        } else if name == "mismatched" {
            (
                StatusCode::OK,
                serde_json::json!({
                    "Name":"other",
                    "Driver":"ployz",
                    "Mountpoint":"/var/lib/ployz-volumes/other",
                    "Status":{"bound_bytes":1073741824,"used_bytes":4096}
                }),
            )
        } else {
            (
                StatusCode::NOT_FOUND,
                serde_json::json!({"message":format!("no such volume: {name}")}),
            )
        }
    } else if method == Method::POST
        && path.ends_with("/volumes/create")
        && String::from_utf8_lossy(&body).contains("rejected")
    {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"message":"create rejected"}),
        )
    } else if method == Method::POST
        && path.ends_with("/volumes/create")
        && let Some(message) = fake.refuse_volume_create.lock().unwrap().clone()
    {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "message": message }),
        )
    } else if method == Method::POST && path.ends_with("/volumes/create") {
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let name = request
            .get("Name")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_owned();
        if name == "unavailable" {
            (
                StatusCode::CREATED,
                serde_json::json!({
                    "Name":"unavailable",
                    "Driver":"ployz",
                    "Mountpoint":"/var/lib/ployz-volumes/unavailable"
                }),
            )
        } else {
            let driver = request
                .get("Driver")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("local");
            let mut observed = serde_json::json!({
                "Name":name,
                "Driver":driver,
                "Mountpoint":format!("/volumes/{name}"),
                "Options":request.get("DriverOpts").cloned().unwrap_or_default(),
                "Labels":request.get("Labels").cloned().unwrap_or_default(),
            });
            if driver == ployz_core::PROVISIONED_VOLUME_DRIVER {
                let bound = request
                    .get("DriverOpts")
                    .and_then(|options| options.get("size"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap()
                    .trim_end_matches('b')
                    .parse::<u64>()
                    .unwrap();
                observed.as_object_mut().unwrap().insert(
                    "Status".into(),
                    serde_json::json!({"bound_bytes":bound,"used_bytes":0}),
                );
            }
            if fake.crash_inside_create.lock().unwrap().remove(&name) {
                let record = observed.as_object_mut().unwrap();
                record.remove("Labels");
                record.remove("Options");
                fake.volumes.lock().unwrap().insert(name, observed);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"message":"plugin ployz exited"})),
                )
                    .into_response();
            }
            let mut response = observed.clone();
            if driver == ployz_core::PROVISIONED_VOLUME_DRIVER {
                // Docker's plugin Create response lacks Status; the following inspect supplies it.
                response.as_object_mut().unwrap().remove("Status");
            }
            fake.volumes.lock().unwrap().insert(name.clone(), observed);
            if fake.fail_after_create.lock().unwrap().contains(&name) {
                fake.fail_inspect_once.lock().unwrap().insert(name);
            }
            (StatusCode::CREATED, response)
        }
    } else if method == Method::POST && (path.ends_with("/start") || path.ends_with("/stop")) {
        if path.ends_with("/start")
            && let Some(barrier) = &fake.start_barrier
        {
            barrier.wait().await;
            barrier.wait().await;
        }
        (StatusCode::NO_CONTENT, serde_json::Value::Null)
    } else if method == Method::DELETE && path.contains("/volumes/") {
        let name = path.rsplit('/').next().unwrap();
        let mounting: Vec<serde_json::Value> = fake
            .named_containers
            .iter()
            .flat_map(|containers| {
                containers
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|container| mounted_volumes(container).any(|mounted| mounted == name))
                    .map(|container| serde_json::json!({"Id": container.get("Id")}))
                    .collect::<Vec<_>>()
            })
            .collect();
        let holders = fake
            .volume_users
            .lock()
            .unwrap()
            .get(name)
            .filter(|holders| !holders.is_empty())
            .cloned()
            .or_else(|| (!mounting.is_empty()).then_some(mounting));
        if let Some(holders) = holders {
            let ids: Vec<String> = holders
                .iter()
                .filter_map(|holder| holder.get("Id").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .collect();
            (
                StatusCode::CONFLICT,
                serde_json::json!({
                    "message": format!("remove {name}: volume is in use - {ids:?}")
                }),
            )
        } else {
            fake.volume_users.lock().unwrap().remove(name);
            fake.volumes.lock().unwrap().remove(name);
            fake.listed_volumes
                .lock()
                .unwrap()
                .retain(|volume| volume["Name"] != name);
            (StatusCode::NO_CONTENT, serde_json::Value::Null)
        }
    } else if method == Method::POST && path.ends_with("/connect") {
        let network = path
            .trim_end_matches("/connect")
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        if let Some(networks) = fake
            .existing_container
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|container| container.pointer_mut("/NetworkSettings/Networks"))
            .and_then(serde_json::Value::as_object_mut)
        {
            networks.insert(network, serde_json::json!({}));
        }
        if fake.connect_raced.load(Ordering::SeqCst) {
            (
                StatusCode::FORBIDDEN,
                serde_json::json!({"message":"endpoint with name api already exists in network ployz"}),
            )
        } else {
            (StatusCode::OK, serde_json::Value::Null)
        }
    } else if method == Method::DELETE && path.contains("/containers/") {
        fake.existing_container.lock().unwrap().take();
        (StatusCode::NO_CONTENT, serde_json::Value::Null)
    } else {
        (
            StatusCode::NOT_FOUND,
            serde_json::json!({"message":format!("unhandled {method} {path}")}),
        )
    };
    (response.0, Json(response.1)).into_response()
}

pub(super) async fn fake_runtime() -> (ContainerRuntime, FakeDocker) {
    fake_runtime_with(FakeDocker::default()).await
}

/// The images in `listed` an image listing's `dangling` and `label` filters keep.
fn listed_images(listed: &[serde_json::Value], query: Option<&str>) -> serde_json::Value {
    let filters = url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
        .find(|(key, _)| key == "filters")
        .map(|(_, filters)| serde_json::from_str::<serde_json::Value>(&filters).unwrap())
        .unwrap_or_default();
    let values = |key: &str| -> Vec<String> {
        filters
            .get(key)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let dangling = values("dangling").iter().any(|value| value == "true");
    let labels = values("label");
    listed
        .iter()
        .filter(|image| {
            !dangling
                || image["RepoTags"]
                    .as_array()
                    .is_none_or(|tags| tags.is_empty())
        })
        .filter(|image| {
            labels.iter().all(|label| {
                let (key, value) = label.split_once('=').unwrap_or((label, ""));
                image
                    .get("Labels")
                    .and_then(|labels| labels.get(key))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|held| value.is_empty() || held == value)
            })
        })
        .map(|image| {
            let mut image = image.clone();
            image.as_object_mut().unwrap().remove("LastTagTime");
            image
        })
        .collect()
}

fn listed_by_labels(
    containers: &BTreeMap<String, serde_json::Value>,
    query: Option<&str>,
) -> serde_json::Value {
    let filters = url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
        .find(|(key, _)| key == "filters")
        .map(|(_, filters)| serde_json::from_str::<serde_json::Value>(&filters).unwrap())
        .unwrap_or_default();
    // Docker lists only running Containers unless asked for all.
    let all = url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
        .any(|(key, value)| key == "all" && (value == "true" || value == "1"));
    let wanted = filters
        .get("label")
        .and_then(serde_json::Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    containers
        .values()
        .filter(|container| {
            all || container
                .pointer("/State/Status")
                .and_then(serde_json::Value::as_str)
                == Some("running")
        })
        .filter_map(|container| {
            let labels = container.pointer("/Config/Labels")?;
            wanted
                .iter()
                .all(|wanted| match wanted.split_once('=') {
                    Some((key, value)) => {
                        labels.get(key).and_then(serde_json::Value::as_str) == Some(value)
                    }
                    None => labels.get(*wanted).is_some(),
                })
                .then(|| serde_json::json!({"Id": container.get("Id"), "Labels": labels}))
        })
        .collect()
}

/// Docker's mark on a volume it made for a Container's anonymous mount.
const ANONYMOUS_VOLUME_LABEL: &str = "com.docker.volume.anonymous";

/// The names of the volumes `container` mounts.
fn mounted_volumes(container: &serde_json::Value) -> impl Iterator<Item = String> + '_ {
    container
        .get("Mounts")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|mount| mount.get("Type").and_then(serde_json::Value::as_str) == Some("volume"))
        .filter_map(|mount| mount.get("Name").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

pub(crate) async fn fake_runtime_with(fake: FakeDocker) -> (ContainerRuntime, FakeDocker) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/{*path}", any(fake_docker))
                .with_state(fake.clone()),
        )
        .into_future(),
    );
    let docker = Docker::connect_with_http(
        &format!("http://{address}"),
        5,
        bollard::API_DEFAULT_VERSION,
    )
    .unwrap();
    let specs = MachineSpecStore::open(std::env::temp_dir().join(format!(
        "ployzd-volume-observation-{}.db",
        MachineId::random()
    )))
    .await
    .unwrap();
    (
        ContainerRuntime::new(LocalDocker::from_client(docker), specs),
        fake,
    )
}

/// A running managed Container made by the real create path, so it carries the
/// labels creation stamps.
pub(super) async fn runtime_with_running_container() -> (ContainerRuntime, FakeDocker, ContainerId)
{
    let (runtime, fake) = fake_runtime_with(FakeDocker {
        create_barrier: Some(Arc::new(tokio::sync::Barrier::new(1))),
        ..Default::default()
    })
    .await;
    let machine = machine();
    let created = runtime
        .lifecycle()
        .create_for_test(
            &machine.id,
            machine.subnet.gateway(),
            ContainerKind::ServiceContainer,
            &Namespace::parse("app").unwrap(),
            &spec_with_sources(vec![]),
        )
        .await
        .unwrap();
    let labels = fake
        .request_bodies
        .lock()
        .unwrap()
        .iter()
        .find(|(path, _)| path.ends_with("/containers/create"))
        .unwrap()
        .1
        .get("Labels")
        .unwrap()
        .clone();
    *fake.existing_container.lock().unwrap() = Some(serde_json::json!({
        "Id": created.container_id,
        "Name": format!("/{}", created.display_name),
        "Config": {"Labels": labels},
        "State": {"Status": "running", "Running": true},
    }));
    (runtime, fake, created.container_id)
}

pub(super) fn machine() -> Machine {
    crate::docker::lifecycle::test_machine(
        MachineId::random(),
        MachineGateway("10.210.0.1".parse().unwrap()),
    )
}

/// The ID this fake gives `image`: the digest it names, or one made from its name, so a
/// pull, an inspect and a Container of one reference agree.
fn image_id(image: &str) -> String {
    use sha2::Digest as _;

    match ployz_core::ImageRef::new(image).digest {
        Some(digest) => digest.to_owned(),
        // Docker takes a bare image ID as itself.
        None if image.starts_with("sha256:") => image.to_owned(),
        None => format!("sha256:{}", hex::encode(sha2::Sha256::digest(image))),
    }
}

/// A volume plugin no test serves: a create that reads a Provisioned Volume's origin
/// through it is refused, as when the plugin is down.
pub(crate) static NO_PLUGIN: std::sync::LazyLock<crate::storage::Plugin> =
    std::sync::LazyLock::new(|| {
        crate::storage::Plugin::at("/nonexistent/ployz-test-plugin.sock".into())
    });

pub(super) fn container_request<'spec, Storage>(
    kind: ContainerKind,
    namespace: &'spec Namespace,
    spec: &'spec ResolvedServiceSpec,
    storage: Storage,
) -> ContainerRequest<'spec, Storage, std::future::Ready<Result<(), Error>>> {
    ContainerRequest {
        deployment_id: None,
        r#gen: ployz_core::Gen::FIRST,
        replaces: None,
        image: crate::docker_image::ImagePull::Policy(crate::docker_image::Credential::Auth(None)),
        creation_key: "test-key",
        kind,
        namespace,
        spec,
        admitted: crate::machine::AdmittedSpec::test_signed(namespace, spec),
        admission: std::future::ready(Ok(())),
        storage,
        plugin: &NO_PLUGIN,
    }
}

pub(crate) fn provisioned_source(name: &str, maximum_bytes: u64) -> VolumeSource {
    let mut source = ployz_core::RawVolumeSource::Provisioned {
        name: DockerVolumeName::parse(name).unwrap(),
        maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
            std::num::NonZeroU64::new(maximum_bytes).unwrap(),
        ),
        labels: BTreeMap::from([("backup".into(), "daily".into())]),
        birth: None,
    }
    .admit()
    .expect("valid volume declaration");
    source.scope_to_namespace(&Namespace::parse("app").unwrap());
    source
}

pub(crate) fn ordinary_source(name: &str) -> VolumeSource {
    let mut source = ployz_core::RawVolumeSource::Ordinary {
        name: DockerVolumeName::parse(name).unwrap(),
        driver: ployz_core::VolumeDriver::parse(
            "example-driver",
            BTreeMap::from([("mode".into(), "safe".into())]),
        )
        .unwrap(),
        labels: BTreeMap::from([("backup".into(), "daily".into())]),
        birth: None,
    }
    .admit()
    .expect("valid volume declaration");
    source.scope_to_namespace(&Namespace::parse("app").unwrap());
    source
}

pub(crate) fn spec_with_sources(sources: Vec<VolumeSource>) -> ResolvedServiceSpec {
    let mut spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": ployz_core::ServiceId::random(),
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "example.test/api", "pull_policy": "missing" },
        "pre_deploy": { "command": ["true"] }
    }))
    .unwrap();
    let volumes = sources
        .into_iter()
        .enumerate()
        .map(|(index, source)| ployz_core::ServiceVolume {
            reference: ployz_core::ServiceVolumeReference::parse(format!("v{index}")).unwrap(),
            source,
        })
        .collect::<Vec<_>>();
    let mounts = volumes
        .iter()
        .enumerate()
        .map(|(index, volume)| ployz_core::ServiceMount {
            volume: volume.reference.clone(),
            target: ployz_core::ContainerPath::parse(format!("/data/{index}")).unwrap(),
            read_only: false,
            no_copy: false,
            subpath: None,
        })
        .collect();
    spec.set_volume_graph(
        ployz_core::ServiceVolumeGraph::parse(volumes, mounts)
            .unwrap()
            .scope_to_namespace(&Namespace::parse("app").unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    spec
}
