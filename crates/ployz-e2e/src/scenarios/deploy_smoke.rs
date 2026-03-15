use crate::error::{Error, Result};
use crate::runner::ScenarioRun;
use crate::support::wait_until;
use std::time::Duration;

const SERVICE_WAIT_TIMEOUT: Duration = Duration::from_secs(180);

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.wait_for_settled_machine_states("founder", &[("founder", "enabled")])?;
    run.ssh_expect_ok_name(
        "founder",
        "ployzd deploy service web --namespace default --image nginx:1.27-alpine",
    )?;
    run.wait_service_container_name("founder", "default", "web")?;
    wait_for_service_http(run, "founder", "default", "web")
}

fn wait_for_service_http(
    run: &ScenarioRun,
    node_name: &str,
    namespace: &str,
    service: &str,
) -> Result<()> {
    let command = format!(
        "sh -lc 'container_id=$(docker ps -q --filter label=dev.ployz.namespace={namespace} --filter label=dev.ployz.service={service} | head -n1); \
         test -n \"$container_id\"; \
         container_ip=$(docker inspect --format \"{{{{range .NetworkSettings.Networks}}}}{{{{.IPAddress}}}}{{{{end}}}}\" \"$container_id\"); \
         test -n \"$container_ip\"; \
         curl -fsS \"http://$container_ip\" | grep -qi nginx'"
    );

    wait_until(SERVICE_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, &command)?;
        Ok(output.status.success())
    })
    .map_err(|error| {
        Error::Message(format!(
            "service '{service}' in namespace '{namespace}' on {node_name} did not serve http: {error}"
        ))
    })
}
