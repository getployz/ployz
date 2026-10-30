use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixListener,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

#[test]
fn dial_stdio_is_hidden_from_daemon_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("dial-stdio")
    );
}

#[test]
fn dial_stdio_bridges_standard_io_to_the_daemon_socket() {
    let root = std::env::temp_dir().join(format!("ployzd-dial-stdio-{}", std::process::id()));
    let socket = root.join("ployz.sock");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        stream.read_to_end(&mut request).unwrap();
        assert_eq!(request, b"ping");
        stream.write_all(b"pong").unwrap();
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["--socket", socket.to_str().unwrap(), "dial-stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"ping").unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"pong");
    server.join().unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dial_stdio_waits_for_a_late_socket_without_printing_errors() {
    let root = std::env::temp_dir().join(format!("ployzd-dial-stdio-wait-{}", std::process::id()));
    let socket = root.join("ployz.sock");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let listener_socket = socket.clone();
    let server = thread::spawn(move || {
        // real-wait: the socket must appear after the ployzd child first dials, and the child signals nothing before.
        thread::sleep(Duration::from_millis(80));
        let listener = UnixListener::bind(&listener_socket).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        stream.read_to_end(&mut request).unwrap();
        assert_eq!(request, b"ping");
        stream.write_all(b"pong").unwrap();
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["--socket", socket.to_str().unwrap(), "dial-stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"ping").unwrap();

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "late socket should succeed, stderr={stderr}"
    );
    assert_eq!(output.stdout, b"pong");
    assert!(
        !stderr.lines().any(|line| line.starts_with("Error:")),
        "successful wait must not print Error: lines, stderr={stderr}"
    );
    server.join().unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dial_stdio_reports_a_message_when_the_socket_is_unreachable() {
    let socket = format!("/tmp/{}/ployz.sock", "x".repeat(200));

    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["--socket", &socket, "dial-stdio"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        !stderr.contains("Os {"),
        "failure must be a message, not a Debug struct, stderr={stderr}"
    );
    assert!(
        !stderr.lines().any(|line| line.starts_with("Error:")),
        "failure must not use Result Termination Debug, stderr={stderr}"
    );
    assert!(
        stderr.to_ascii_lowercase().contains("socket")
            || stderr.to_ascii_lowercase().contains("connect"),
        "failure must mention the socket, stderr={stderr}"
    );
}

#[test]
fn dial_stdio_names_the_refusal_when_a_refusing_daemon_closes_it_unanswered() {
    let root =
        std::env::temp_dir().join(format!("ployzd-dial-stdio-refused-{}", std::process::id()));
    let socket = root.join("ployz.sock");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let refusal = "refusing to change the retained WireGuard interface: the existing device listen port differs from the mesh port; safe recovery: delete ployz-wg";
    let listener = UnixListener::bind(&socket).unwrap();
    let refused = root.join("refused");
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_millis(200));
        fs::write(refused, format!("{refusal}\n")).unwrap();
        drop(stream);
    });

    // A client's request sits unread when the start closes it, which resets it.
    let mut child = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["--socket", socket.to_str().unwrap(), "dial-stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    drop(input);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "stderr={stderr}");
    assert_eq!(stderr.trim(), format!("ployzd refused to start: {refusal}"));
    server.join().unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dial_stdio_names_no_refusal_after_a_daemon_that_answered() {
    let root =
        std::env::temp_dir().join(format!("ployzd-dial-stdio-answered-{}", std::process::id()));
    let socket = root.join("ployz.sock");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("refused"), "refusing to change\n").unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"pong").unwrap();
    });

    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["--socket", socket.to_str().unwrap(), "dial-stdio"])
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"pong");
    server.join().unwrap();
    fs::remove_dir_all(root).unwrap();
}
