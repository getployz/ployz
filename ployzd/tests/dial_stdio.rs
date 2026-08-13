use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixListener,
    process::{Command, Stdio},
    thread,
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
