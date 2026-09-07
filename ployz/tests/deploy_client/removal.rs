use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn removal_tty_retry_enter_eof_and_ctrl_c_precede_mutation() {
    for answer in [b"\n".as_slice(), b"\x04", b"\x03", b"app/web\n"] {
        let machine = machine('a', "one");
        let service = DeployService::new(machine.clone()).with_dropped_observations();
        let mut web = spec("web");
        add_named_volume(&mut web, "data");
        service
            .listed_containers()
            .lock()
            .unwrap()
            .push(running_container(&machine, &web));
        let mutations = service.mutating_rpcs();
        let (address, server) = listening(service).await;
        let command = shell_words::join([
            env!("CARGO_BIN_EXE_ployz"),
            "--connect",
            &format!("tcp://{address}"),
            "rm",
            "web",
            "--volumes",
        ]);
        let mut child = tokio::process::Command::new("script")
            .args(["--quiet", "--return", "--command", &command, "/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut transcript = Vec::new();
        for (expected, input) in [
            ("Type \"app/web\" to confirm: ", b"wrong\n".as_slice()),
            ("Type \"app/web\" to confirm: ", answer),
        ] {
            let mut part = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !String::from_utf8_lossy(&part).contains(expected) {
                    part.push(
                        stdout
                            .read_u8()
                            .await
                            .expect("prompt must arrive before exit"),
                    );
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "prompt must not hang: {} / {}",
                    String::from_utf8_lossy(&transcript),
                    String::from_utf8_lossy(&part)
                )
            });
            transcript.extend(part);
            stdin.write_all(input).await.unwrap();
            stdin.flush().await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), stdout.read_to_end(&mut transcript))
            .await
            .expect("cancellation must not leave a stdin reader blocking exit")
            .unwrap();
        let status = child.wait().await.unwrap();
        let text = String::from_utf8_lossy(&transcript);
        assert!(text.contains("Names did not match"), "{text}");
        if answer == b"app/web\n" {
            // The fake Machine deliberately refuses volume removal after removing the Service.
            assert!(!status.success(), "{text}");
            assert!(mutations.load(Ordering::SeqCst) > 0);
            assert!(text.contains("Remove\tapp/web"), "{text}");
            assert!(!text.contains("Cancelled"), "{text}");
            assert!(
                text.contains("app_data") && text.contains("unused"),
                "{text}"
            );
            assert!(text.contains("removals failed or were omitted"), "{text}");
        } else {
            assert!(status.success(), "{text}");
            assert_eq!(mutations.load(Ordering::SeqCst), 0);
            assert!(text.contains("Cancelled. No changes made."), "{text}");
        }
        server.abort();
    }
}

#[tokio::test]
async fn removal_non_tty_yes_environment_cannot_accept_volume_loss() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone());
    let mut web = spec("web");
    add_named_volume(&mut web, "data");
    service
        .listed_containers()
        .lock()
        .unwrap()
        .push(running_container(&machine, &web));
    let mutations = service.mutating_rpcs();
    let (address, server) = listening(service).await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "rm",
            "web",
            "--volumes",
        ])
        .env("PLOYZ_AUTO_CONFIRM", "true")
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--accept-volume-loss app_data"), "{stderr}");
    assert!(stderr.contains(&format!("tcp://{address}")), "{stderr}");
    server.abort();
}

#[tokio::test]
async fn absent_project_reports_observation_and_leaves_machine_unchanged() {
    let service = DeployService::new(machine('a', "one"));
    let mutations = service.mutating_rpcs();
    let (address, server) = listening(service).await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "project",
            "rm",
            "absent",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "Project 'absent' was not found in this Cluster observation. No changes made."
        ),
        "{stderr}"
    );
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn service_volume_removal_proceeds_when_an_unrelated_machine_is_omitted() {
    let owner = machine('a', "owner");
    let mut unrelated = machine('b', "unrelated");
    unrelated.membership = ployz_core::MembershipObservation::Down;
    let service = DeployService::new(owner.clone())
        .with_machines(vec![owner.clone(), unrelated])
        .with_dropped_observations();
    let mut web = spec("web");
    add_named_volume(&mut web, "data");
    service
        .listed_containers()
        .lock()
        .unwrap()
        .push(running_container(&owner, &web));
    let mutations = service.mutating_rpcs();
    let (address, server) = listening(service).await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "rm",
            "web",
            "--volumes",
            "--accept-volume-loss",
            "app_data",
        ])
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(mutations.load(Ordering::SeqCst) > 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("Remove\tapp/web"), "{stdout}");
    // The fake Machine refuses the volume RPC; this must be an execution failure, not a preflight refusal.
    assert!(!output.status.success());
    assert!(
        stderr.contains("app_data") && stderr.contains("unused"),
        "{stderr}"
    );
    assert!(stderr.contains("was omitted"), "{stderr}");
    assert!(!stderr.contains("No changes made"), "{stderr}");
    server.abort();
}
