//! Owned transport stand-in. It records image identities and RPCs, never proves
//! that Docker built or ran an image; build_layer3 supplies that evidence.

use ployz_build::{
    Output,
    remote::{self, Event, Input, Outcome},
};
use ployz_core::{MachineId, MachineImages, OpaquePayload, PullImageFromMachineRequest};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

#[derive(Default)]
pub struct BuildFixture {
    pub terminal: Mutex<Option<Outcome>>,
    pub platforms: Mutex<Option<Vec<String>>>,
    pub workers: Mutex<BTreeMap<MachineId, Vec<String>>>,
    pub definitions: Mutex<Vec<remote::Definition>>,
    pub stores: Mutex<BTreeMap<MachineId, MachineImages>>,
    pub opened: Mutex<Vec<MachineId>>,
    pub pulls: Mutex<Vec<(MachineId, PullImageFromMachineRequest)>>,
    pub pull_failures: Mutex<BTreeMap<MachineId, ployz_core::RpcError>>,
}

impl BuildFixture {
    pub fn images(&self, machine: MachineId) -> MachineImages {
        self.stores
            .lock()
            .unwrap()
            .get(&machine)
            .cloned()
            .unwrap_or(MachineImages {
                containerd_store: true,
                images: Vec::new(),
            })
    }

    pub fn open(&self, machine: &ployz_core::Machine) -> ployz_core::ImageIngestOpened {
        self.opened.lock().unwrap().push(machine.id);
        ployz_core::ImageIngestOpened {
            destination: ployz_core::ImageIngestDestination {
                management_address: machine.management_address(),
                port: ployz_core::UNREGISTRY_PORT,
            },
        }
    }

    pub fn pull(
        &self,
        machine: MachineId,
        pull: PullImageFromMachineRequest,
    ) -> ployz_core::RpcResponse {
        let failure = self.pull_failures.lock().unwrap().get(&machine).cloned();
        if failure.is_none() {
            // The destination now holds exactly the requested variant, as a
            // real pull of one platform leaves it: identity intact, other
            // variants absent.
            let (repo_tags, id) = match pull.image.rsplit_once('@') {
                Some((_, digest)) => (pull.tag.iter().cloned().collect(), digest.to_owned()),
                None => (
                    vec![pull.image.clone()],
                    format!("sha256:{}", "f".repeat(64)),
                ),
            };
            let mut stores = self.stores.lock().unwrap();
            let store = stores.entry(machine).or_insert(MachineImages {
                containerd_store: true,
                images: Vec::new(),
            });
            let platforms = vec![pull.platform.clone()];
            match store.images.iter_mut().find(|stored| stored.id == id) {
                Some(stored) => {
                    stored.platforms.extend(platforms);
                    stored.repo_tags.extend(repo_tags);
                }
                None => store.images.push(ployz_core::ImageSummary {
                    id,
                    repo_tags,
                    created: 0,
                    size: 1,
                    containers: 0,
                    platforms,
                }),
            }
        }
        self.pulls.lock().unwrap().push((machine, pull));
        failure.map_or_else(
            || ployz_core::RpcResponse::from(ployz_core::ImagePulled {}),
            ployz_core::RpcResponse::from,
        )
    }

    #[expect(clippy::result_large_err)] // tonic fixes the public RPC error type.
    pub fn stream(
        self: Arc<Self>,
        request: Request<tonic::Streaming<OpaquePayload>>,
    ) -> Result<Response<ReceiverStream<Result<OpaquePayload, Status>>>, Status> {
        let machine_id =
            MachineId::parse(request.metadata().get("machine").unwrap().to_str().unwrap())
                .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let (sender, receiver) = mpsc::channel(2);
        tokio::spawn(async move {
            let mut request = request.into_inner();
            let frame = remote::decode(&request.message().await.unwrap().unwrap()).unwrap();
            if let Input::Check(targets) = frame {
                let workers = self
                    .workers
                    .lock()
                    .unwrap()
                    .get(&machine_id)
                    .cloned()
                    .unwrap_or_else(|| vec!["linux/amd64".into()]);
                let unsupported = targets
                    .iter()
                    .flat_map(|target| &target.platforms)
                    .find(|platform| !workers.contains(platform));
                let outcome =
                    unsupported.map_or(Outcome::CapabilitiesChecked { machine_id }, |platform| {
                        Outcome::Failed {
                            stage: ployz_build::Stage::Preparation,
                            message: format!("the running BuildKit worker cannot build {platform}"),
                            work: Default::default(),
                        }
                    });
                sender
                    .send(Ok(remote::encode(&Event::Admitted {
                        machine_id,
                        active_timeout: ployz_build::EXECUTION_TIMEOUT,
                    })
                    .unwrap()))
                    .await
                    .unwrap();
                sender
                    .send(Ok(remote::encode(&Event::Finished(outcome)).unwrap()))
                    .await
                    .unwrap();
                return;
            }
            let Input::Start(definition) = frame else {
                panic!("expected Build definition");
            };
            let offset = {
                let mut definitions = self.definitions.lock().unwrap();
                let offset = definitions
                    .iter()
                    .map(|definition| definition.targets.len())
                    .sum::<usize>();
                definitions.push(definition.clone());
                offset
            };
            sender
                .send(Ok(remote::encode(&Event::Admitted {
                    machine_id,
                    active_timeout: ployz_build::EXECUTION_TIMEOUT,
                })
                .unwrap()))
                .await
                .unwrap();
            let mut upload = remote::Upload::new().unwrap();
            loop {
                let payload = request.message().await.unwrap().unwrap();
                let frame: Input = remote::decode(&payload).unwrap();
                let finish = matches!(frame, Input::Finish);
                upload.accept(frame).unwrap();
                if finish {
                    break;
                }
            }
            let terminal = self.terminal.lock().unwrap().clone();
            let outcome = terminal.unwrap_or_else(|| match definition.output {
                Output::Validate => Outcome::Validated { machine_id },
                Output::Registry => Outcome::Published { machine_id },
                Output::Load => {
                    let images = definition
                        .targets
                        .iter()
                        .enumerate()
                        .map(|(index, _)| ployz_build::BuiltImage {
                            reference: format!(
                                "sha256:{}",
                                (offset + index + 1).to_string().repeat(64)
                            ),
                            tags: vec!["registry.invalid/shared:latest".into()],
                            platforms: self
                                .platforms
                                .lock()
                                .unwrap()
                                .clone()
                                .unwrap_or_else(|| vec!["linux/amd64".into()]),
                            location: "unix:///var/run/docker.sock".into(),
                        })
                        .collect::<Vec<_>>();
                    let mut stores = self.stores.lock().unwrap();
                    let store = stores.entry(machine_id).or_insert(MachineImages {
                        containerd_store: true,
                        images: Vec::new(),
                    });
                    // Keep content after another Build overwrites the requested tag.
                    for stored in &mut store.images {
                        stored.repo_tags.clear();
                    }
                    store
                        .images
                        .extend(images.iter().map(|image| ployz_core::ImageSummary {
                            id: image.reference.clone(),
                            repo_tags: image.tags.clone(),
                            created: 0,
                            size: 1,
                            containers: 0,
                            platforms: image.platforms.clone(),
                        }));
                    Outcome::Images { machine_id, images }
                }
            });
            let _ = sender
                .send(Ok(remote::encode(&Event::Finished(outcome)).unwrap()))
                .await;
        });
        Ok(Response::new(ReceiverStream::new(receiver)))
    }
}
