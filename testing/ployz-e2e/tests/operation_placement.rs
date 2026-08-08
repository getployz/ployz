//! Three-machine public-seam proof of placement bids: spread, sticky,
//! pins with loud stacking and shortfall, `--machine any`, global mode with
//! host-published ports, volume affinity, and the typed placement refusals.

#[path = "operation_placement/support.rs"]
mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU16;

use bollard::Docker;
use ployz::commands::SshTarget;
use ployz::mesh::http::JsonReply;
use ployz_core::HealthGatePolicy;
use ployz_core::corrosion::{
    CorrosionNamespaceName, CorrosionServiceName, HostPortBinding, HostPortBindings,
    HostPortProtocol, ServicePlacement,
};
use ployz_core::deploy::{
    ContainerMountPath, ContainerRuntimeSpec, ImageReference, ServiceVolumeMount, VolumeName,
};
use ployz_core::ids::MachineRowId;
use ployz_core::placement::PlacementRefusal;
use ployz_core::{DeployRefusal, DeployRequest};
use ployz_e2e::dind as deploy_support;
use ployz_e2e::dind::{
    DindCluster, DindClusterSpec, DindMachine, MachineSpec, artifact_dir, connect_docker,
    e2e_enabled, exec_ok, keep_requested, machine_image, require,
};

use support::require_contains;

const NAMESPACE: &str = "production";
const SERVICE: &str = "web";
const SECRET_NAME: &str = "OPERATION_E2E_SECRET";
const SECRET_VALUE: &str = "sentinel-operation-placement-secret";
const FIRST_BODY: &str = "Welcome to nginx";
const SECOND_BODY: &str = "ployz-placement-second-revision";
const VOLUME_NAMESPACE: &str = "storage";
const VOLUME_SERVICE: &str = "vol";
const VOLUME_FIRST_BODY: &str = "ployz-placement-volume-first";
const VOLUME_SECOND_BODY: &str = "ployz-placement-volume-second";
const PINNED_NAMESPACE: &str = "pinned";
const PUBLISHED_HOST_PORT: u16 = 8_088;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn placement_bids_drive_spread_sticky_pins_global_and_volume_affinity() {
    if !e2e_enabled() {
        eprintln!("skipping operation-placement DinD proof; set PLOYZ_DIND_E2E=1 to enable it");
        return;
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    panic!("the pinned operation-placement proof supports only Linux x86_64");

    let docker = connect_docker().expect("connect to Docker for operation-placement proof");
    let cluster = DindCluster::provision(
        &docker,
        DindClusterSpec {
            artifact_dir: artifact_dir(),
            machines: vec![
                MachineSpec {
                    image: machine_image(),
                },
                MachineSpec {
                    image: machine_image(),
                },
                MachineSpec {
                    image: machine_image(),
                },
            ],
        },
    )
    .await
    .expect("provision operation-placement machines");

    let result = exercise_operation_placement(&docker, &cluster).await;
    if let Err(error) = &result {
        match cluster.capture_evidence().await {
            Ok(path) => eprintln!(
                "operation-placement evidence captured under {}",
                path.display()
            ),
            Err(capture_error) => {
                eprintln!("operation-placement evidence capture failed: {capture_error}")
            }
        }
        eprintln!("operation-placement proof failed: {error}");
    }
    if keep_requested() {
        eprintln!(
            "retaining DinD run {} because PLOYZ_DIND_KEEP=1",
            cluster.run_id()
        );
    } else {
        cluster
            .teardown()
            .await
            .expect("tear down operation-placement run");
    }
    result.unwrap_or_else(|error| panic!("operation-placement proof failed: {error}"));
}

/// One cluster member the scenario can address by roster id, ployz machine
/// name, operator SSH target, and DinD container.
struct Member<'a> {
    id: MachineRowId,
    name: String,
    target: SshTarget,
    machine: &'a DindMachine,
}

fn member_for<'a>(members: &'a [Member<'a>], id: &MachineRowId) -> Result<&'a Member<'a>, String> {
    members
        .iter()
        .find(|member| &member.id == id)
        .ok_or_else(|| format!("machine {id} is not a provisioned cluster member"))
}

fn distinct_machines(rows: &support::PlacedRows) -> BTreeSet<MachineRowId> {
    rows.containers
        .iter()
        .map(|(machine, _)| machine.clone())
        .collect()
}

async fn exercise_operation_placement(
    docker: &Docker,
    cluster: &DindCluster,
) -> Result<(), String> {
    let [m1, m2, m3] = cluster.machines() else {
        return Err("operation-placement proof requires exactly three machines".to_owned());
    };
    let operator = deploy_support::found_and_join(docker, m1, &[m2, m3]).await?;
    let [j1, j2] = operator.joiners.as_slice() else {
        return Err("operation-placement proof expects two joined machines".to_owned());
    };
    let image = deploy_support::start_mutable_registry(docker, m1, &[m2, m3]).await?;
    let founder_target = operator.founder_target.clone();
    let members = [
        Member {
            id: operator.founder_machine_id.clone(),
            name: deploy_support::FOUNDER_NAME.to_owned(),
            target: founder_target.clone(),
            machine: m1,
        },
        Member {
            id: j1.machine_id.clone(),
            name: j1.name.clone(),
            target: j1.target.clone(),
            machine: m2,
        },
        Member {
            id: j2.machine_id.clone(),
            name: j2.name.clone(),
            target: j2.target.clone(),
            machine: m3,
        },
    ];
    let env = format!("{SECRET_NAME}={SECRET_VALUE}");
    let hostname = format!("{SERVICE}.{NAMESPACE}.internal");
    deploy_support::create_namespace(&operator, NAMESPACE, &founder_target)?;

    // Two replicas spread across two distinct machines; the driver gathers a
    // bid from every roster machine including itself.
    let spread = deploy_support::run_cli(
        &operator,
        &[
            "deploy",
            NAMESPACE,
            SERVICE,
            &image,
            "--env",
            &env,
            "--replicas",
            "2",
            "--target",
            founder_target.as_str(),
        ],
    )?;
    let spread_op = deploy_support::parse_deploy_operation(&spread, "spread deploy", SECRET_VALUE)?;
    require_contains(
        "spread deploy progress",
        &String::from_utf8_lossy(&spread.stdout),
        &[
            "placement gathered: 3 bid(s), 0 silent",
            "placement picked: 2 replica(s) on 2 machine(s)",
            "terminal deploy completed",
        ],
    )?;
    let spread_rows =
        support::wait_for_placed_rows(docker, m1, &j1.api_address, SERVICE, &spread_op, 2).await?;
    let spread_set = distinct_machines(&spread_rows);
    require(
        spread_set.len() == 2,
        format!("replicas stacked instead of spreading: {spread_set:?}"),
    )?;
    assert_replicas_serve(
        docker,
        &members,
        m2,
        j1.dns_address,
        &hostname,
        &spread_rows,
        FIRST_BODY,
    )
    .await?;
    let replay =
        deploy_support::assert_cluster_wide_operation_replay(&operator, &spread_op, SECRET_VALUE)?;
    require_contains(
        "spread replay",
        &replay,
        &[
            "placement gathered: 3 bid(s), 0 silent",
            "placement picked: 2 replica(s) on 2 machine(s)",
        ],
    )?;
    deploy_support::assert_driver_local_evidence_is_secret_free(
        docker,
        m1,
        &spread_op,
        SECRET_VALUE,
    )
    .await?;
    for non_driver in [m2, m3] {
        support::assert_machine_evidence_is_secret_free(docker, non_driver, SECRET_VALUE).await?;
    }

    // A flag-less redeploy of a new revision sticks to the incumbent machines.
    deploy_support::push_second_revision(docker, m1, &image, SECOND_BODY).await?;
    let sticky = deploy_support::run_cli(
        &operator,
        &[
            "deploy",
            NAMESPACE,
            SERVICE,
            &image,
            "--env",
            &env,
            "--target",
            founder_target.as_str(),
        ],
    )?;
    let sticky_op = deploy_support::parse_deploy_operation(&sticky, "sticky deploy", SECRET_VALUE)?;
    require_contains(
        "sticky deploy progress",
        &String::from_utf8_lossy(&sticky.stdout),
        &["placement picked: 2 replica(s) on 2 machine(s)"],
    )?;
    let sticky_rows =
        support::wait_for_placed_rows(docker, m1, &j1.api_address, SERVICE, &sticky_op, 2).await?;
    require(
        distinct_machines(&sticky_rows) == spread_set,
        format!(
            "sticky redeploy moved replicas: {:?} != {spread_set:?}",
            distinct_machines(&sticky_rows)
        ),
    )?;
    assert_replicas_serve(
        docker,
        &members,
        m2,
        j1.dns_address,
        &hostname,
        &sticky_rows,
        SECOND_BODY,
    )
    .await?;
    deploy_support::assert_first_revision_container_is_gone(docker, &[m1, m2, m3], &spread_op)
        .await?;

    // Three replicas over a two-machine pin set: the third machine is
    // eliminated for the pin, replicas stack 2+1, and the shortfall is loud.
    let [pin_a, pin_b, unpinned] = &members;
    let pinned = deploy_support::run_cli(
        &operator,
        &[
            "deploy",
            NAMESPACE,
            SERVICE,
            &image,
            "--env",
            &env,
            "--replicas",
            "3",
            "--machine",
            &pin_a.name,
            "--machine",
            &pin_b.name,
            "--target",
            founder_target.as_str(),
        ],
    )?;
    let pinned_op = deploy_support::parse_deploy_operation(&pinned, "pinned deploy", SECRET_VALUE)?;
    require_contains(
        "pinned deploy progress",
        &String::from_utf8_lossy(&pinned.stdout),
        &[
            "placement picked: 3 replica(s) on 2 machine(s)",
            "(2 replicas)",
            &format!("eliminated {}: outside the pin set", unpinned.name),
            "shortfall: requested 3, placed 2",
        ],
    )?;
    let pinned_rows =
        support::wait_for_placed_rows(docker, m1, &j1.api_address, SERVICE, &pinned_op, 3).await?;
    let pin_ids = [pin_a.id.clone(), pin_b.id.clone()]
        .into_iter()
        .collect::<BTreeSet<_>>();
    require(
        pinned_rows.service.pinned_machines == pin_ids,
        format!(
            "service row did not record the pin set: {:?}",
            pinned_rows.service.pinned_machines
        ),
    )?;
    let mut stacking = BTreeMap::new();
    for (machine, _) in &pinned_rows.containers {
        *stacking.entry(machine.clone()).or_insert(0_usize) += 1;
    }
    let mut sizes = stacking.values().copied().collect::<Vec<_>>();
    sizes.sort_unstable();
    require(
        stacking.keys().all(|machine| pin_ids.contains(machine)) && sizes == [1, 2],
        format!("pinned replicas did not stack 2+1 across the pin set: {stacking:?}"),
    )?;

    // `--machine any` clears the pins and three replicas spread one per machine.
    let any = deploy_support::run_cli(
        &operator,
        &[
            "deploy",
            NAMESPACE,
            SERVICE,
            &image,
            "--env",
            &env,
            "--replicas",
            "3",
            "--machine",
            "any",
            "--target",
            founder_target.as_str(),
        ],
    )?;
    let any_op = deploy_support::parse_deploy_operation(&any, "unpinned deploy", SECRET_VALUE)?;
    let any_stdout = String::from_utf8_lossy(&any.stdout).into_owned();
    require_contains(
        "unpinned deploy progress",
        &any_stdout,
        &["placement picked: 3 replica(s) on 3 machine(s)"],
    )?;
    require(
        !any_stdout.contains("shortfall:"),
        format!("full spread reported a shortfall: {any_stdout}"),
    )?;
    let any_rows =
        support::wait_for_placed_rows(docker, m1, &j1.api_address, SERVICE, &any_op, 3).await?;
    require(
        distinct_machines(&any_rows).len() == 3 && any_rows.service.pinned_machines.is_empty(),
        format!(
            "`--machine any` did not clear the pins and spread: machines={:?} pins={:?}",
            distinct_machines(&any_rows),
            any_rows.service.pinned_machines
        ),
    )?;

    // `-p` without global mode is refused by the CLI before any request.
    let publish_refused = deploy_support::run_cli(
        &operator,
        &[
            "deploy",
            NAMESPACE,
            SERVICE,
            &image,
            "--env",
            &env,
            "-p",
            "8088:80",
            "--target",
            founder_target.as_str(),
        ],
    )?;
    require(
        !publish_refused.status.success(),
        "publishing a host port off-global must fail".to_owned(),
    )?;
    require_contains(
        "off-global publish refusal",
        &String::from_utf8_lossy(&publish_refused.stderr),
        &["published host ports are legal only on global services"],
    )?;
    require(
        !String::from_utf8_lossy(&publish_refused.stdout).contains("accepted operation"),
        "a refused publish flag must not send a deploy request".to_owned(),
    )?;

    // Global mode runs one container on every live machine and binds the
    // published host port on each machine's own address.
    let global = deploy_support::run_cli(
        &operator,
        &[
            "deploy",
            NAMESPACE,
            SERVICE,
            &image,
            "--env",
            &env,
            "--mode",
            "global",
            "-p",
            "8088:80",
            "--target",
            founder_target.as_str(),
        ],
    )?;
    let global_op = deploy_support::parse_deploy_operation(&global, "global deploy", SECRET_VALUE)?;
    require_contains(
        "global deploy progress",
        &String::from_utf8_lossy(&global.stdout),
        &["placement picked: 3 replica(s) on 3 machine(s)"],
    )?;
    let global_rows =
        support::wait_for_placed_rows(docker, m1, &j1.api_address, SERVICE, &global_op, 3).await?;
    require(
        distinct_machines(&global_rows).len() == 3,
        format!(
            "global mode did not land one container per machine: {:?}",
            global_rows.containers
        ),
    )?;
    let ServicePlacement::Global { host_ports } = &global_rows.service.placement else {
        return Err(format!(
            "service row did not record global placement: {:?}",
            global_rows.service.placement
        ));
    };
    let expected_ports = HostPortBindings::try_new([HostPortBinding {
        host_port: NonZeroU16::new(PUBLISHED_HOST_PORT)
            .ok_or_else(|| "published host port must be nonzero".to_owned())?,
        container_port: NonZeroU16::new(80)
            .ok_or_else(|| "container port must be nonzero".to_owned())?,
        protocol: HostPortProtocol::Tcp,
    }])
    .map_err(|error| error.to_string())?;
    require(
        host_ports == &expected_ports,
        format!("service row recorded the wrong host ports: {host_ports:?}"),
    )?;
    for member in &members {
        support::wait_for_http_body(
            docker,
            m1,
            &format!("http://{}:{PUBLISHED_HOST_PORT}/", member.machine.bridge_ip),
            SECOND_BODY,
        )
        .await?;
    }

    // Volume service, zero holders: a normal pick creates the volume in-op.
    // The CLI carries no volume flag, so these deploys drive the same
    // operator HTTP seam with a typed request.
    deploy_support::create_namespace(&operator, VOLUME_NAMESPACE, &founder_target)?;
    let IpAddr::V4(registry_ip) = m1.bridge_ip else {
        return Err("operation-placement registry requires an IPv4 DinD bridge".to_owned());
    };
    let volume_image = format!(
        "{registry_ip}:{}/volume-http:latest",
        deploy_support::REGISTRY_PORT
    );
    deploy_support::push_second_revision(docker, m1, &volume_image, VOLUME_FIRST_BODY).await?;
    let volume_request = volume_deploy_request(&volume_image)?;
    let config_home = operator.config_home();
    let first_volume =
        match support::mesh_deploy(&config_home, &founder_target, &volume_request).await? {
            JsonReply::Success(accepted) => accepted,
            JsonReply::Refused(refusal) => {
                return Err(format!("first volume deploy was refused: {refusal:?}"));
            }
        };
    let first_volume_watch = deploy_support::run_cli(
        &operator,
        &[
            "ops",
            "watch",
            first_volume.operation_id.as_str(),
            "--target",
            founder_target.as_str(),
        ],
    )?;
    deploy_support::require_success(&first_volume_watch, "first volume deploy watch")?;
    require_contains(
        "first volume deploy progress",
        &String::from_utf8_lossy(&first_volume_watch.stdout),
        &[
            "placement picked: 1 replica(s) on 1 machine(s)",
            "terminal deploy completed",
        ],
    )?;
    let volume_rows = support::wait_for_placed_rows(
        docker,
        m1,
        &j1.api_address,
        VOLUME_SERVICE,
        &first_volume.operation_id,
        1,
    )
    .await?;
    let [(holder_id, _)] = volume_rows.containers.as_slice() else {
        return Err(format!(
            "volume service did not converge to one container: {:?}",
            volume_rows.containers
        ));
    };
    let holder = member_for(&members, holder_id)?;
    for member in &members {
        let holds = support::machine_holds_data_volume(docker, member.machine).await?;
        require(
            holds == (member.id == *holder_id),
            format!(
                "data volume presence on {} was {holds}; holder is {}",
                member.name, holder.name
            ),
        )?;
    }

    // One visible holder: the redeploy pins to it and the other machines
    // are eliminated for not holding the data volume.
    deploy_support::push_second_revision(docker, m1, &volume_image, VOLUME_SECOND_BODY).await?;
    let second_volume =
        match support::mesh_deploy(&config_home, &founder_target, &volume_request).await? {
            JsonReply::Success(accepted) => accepted,
            JsonReply::Refused(refusal) => {
                return Err(format!(
                    "holder-pinned volume deploy was refused: {refusal:?}"
                ));
            }
        };
    let second_volume_watch = deploy_support::run_cli(
        &operator,
        &[
            "ops",
            "watch",
            second_volume.operation_id.as_str(),
            "--target",
            founder_target.as_str(),
        ],
    )?;
    deploy_support::require_success(&second_volume_watch, "holder-pinned volume deploy watch")?;
    require_contains(
        "holder-pinned volume deploy progress",
        &String::from_utf8_lossy(&second_volume_watch.stdout),
        &[
            "placement picked: 1 replica(s) on 1 machine(s)",
            "does not hold the data volume",
            "terminal deploy completed",
        ],
    )?;
    let second_volume_rows = support::wait_for_placed_rows(
        docker,
        m1,
        &j1.api_address,
        VOLUME_SERVICE,
        &second_volume.operation_id,
        1,
    )
    .await?;
    require(
        matches!(
            second_volume_rows.containers.as_slice(),
            [(machine, _)] if machine == holder_id
        ),
        format!(
            "volume redeploy left the holder {}: {:?}",
            holder.name, second_volume_rows.containers
        ),
    )?;

    // Dark plausible holder: with the holder's daemon stopped, the volume
    // redeploy is a typed refusal naming the dark machine.
    exec_ok(
        docker,
        holder.machine,
        &["systemctl", "stop", "ployzd-api.service"],
    )
    .await?;
    let live = members
        .iter()
        .find(|member| member.id != *holder_id)
        .ok_or_else(|| "no live machine remains beside the holder".to_owned())?;
    let dark_reply = support::mesh_deploy(&config_home, &live.target, &volume_request).await?;
    let dark_refusal = match dark_reply {
        JsonReply::Refused(refusal) => refusal,
        JsonReply::Success(accepted) => {
            return Err(format!(
                "dark-holder volume deploy was accepted as {}",
                accepted.operation_id
            ));
        }
    };
    let DeployRefusal::Placement {
        refusal: PlacementRefusal::DarkVolumeHolder { machines },
    } = dark_refusal
    else {
        return Err(format!(
            "expected the dark-holder refusal, got {dark_refusal:?}"
        ));
    };
    require(
        machines
            .iter()
            .map(|machine| machine.machine_id.clone())
            .collect::<Vec<_>>()
            == vec![holder_id.clone()],
        format!("dark-holder refusal named {machines:?}, expected {holder_id}"),
    )?;

    // Zero eligible bidders: a pin set holding only the dark machine leaves
    // no survivor; the refusal names each elimination.
    deploy_support::create_namespace(&operator, PINNED_NAMESPACE, &live.target)?;
    let zero_bidders = deploy_support::run_cli(
        &operator,
        &[
            "deploy",
            PINNED_NAMESPACE,
            "sink",
            &image,
            "--machine",
            &holder.name,
            "--target",
            live.target.as_str(),
        ],
    )?;
    require(
        !zero_bidders.status.success(),
        "a deploy pinned to a dark machine must be refused".to_owned(),
    )?;
    require_contains(
        "zero-bidder refusal",
        &String::from_utf8_lossy(&zero_bidders.stderr),
        &["no machine can host this deploy", "outside the pin set"],
    )?;
    require(
        !String::from_utf8_lossy(&zero_bidders.stdout).contains("accepted operation"),
        "a refused deploy must not accept an operation".to_owned(),
    )?;
    Ok(())
}

/// For every placed container: cluster DNS resolves the service hostname to
/// its IP, and the machine hosting it serves the expected body at that IP.
async fn assert_replicas_serve(
    docker: &Docker,
    members: &[Member<'_>],
    dns_client: &DindMachine,
    resolver: Ipv4Addr,
    hostname: &str,
    rows: &support::PlacedRows,
    expected_body: &str,
) -> Result<(), String> {
    for (machine_id, ip) in &rows.containers {
        let member = member_for(members, machine_id)?;
        deploy_support::assert_dns_and_http(
            docker,
            dns_client,
            member.machine,
            resolver,
            hostname,
            *ip,
            expected_body,
        )
        .await?;
    }
    Ok(())
}

/// The typed volume-bearing deploy request the CLI cannot yet express.
fn volume_deploy_request(image: &str) -> Result<DeployRequest, String> {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: VolumeName::try_new("data").map_err(|error| error.to_string())?,
        target: ContainerMountPath::try_new("/data").map_err(|error| error.to_string())?,
    }];
    Ok(DeployRequest {
        namespace_name: CorrosionNamespaceName::try_new(VOLUME_NAMESPACE)
            .map_err(|error| error.to_string())?,
        service_name: CorrosionServiceName::try_new(VOLUME_SERVICE)
            .map_err(|error| error.to_string())?,
        image: ImageReference::try_new(image).map_err(|error| error.to_string())?,
        runtime,
        health_gate: HealthGatePolicy::Enforce,
        placement: None,
        machines: None,
    })
}
