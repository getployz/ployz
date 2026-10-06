//! Force real backend writes after listener destruction, with retained and missing receivers.

use super::*;
use std::{
    fmt::Debug,
    io::Write,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Debug, Default)]
struct Gate {
    phase: AtomicU8,
    calls: AtomicUsize,
    dropped: AtomicBool,
    dropped_while_held: AtomicBool,
}

impl Gate {
    fn arm(&self) {
        self.phase.store(1, Ordering::SeqCst);
    }
    fn release(&self) {
        self.phase.store(3, Ordering::SeqCst);
    }
    fn entered(&self) {
        until(|| self.phase.load(Ordering::SeqCst) == 2);
    }
}

#[derive(Debug)]
struct DropProbe(Arc<Gate>);
impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0
            .dropped_while_held
            .store(self.0.phase.load(Ordering::SeqCst) == 2, Ordering::SeqCst);
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct ProbedWriter<W> {
    writer: W,
    gate: Arc<Gate>,
    _after_writer: DropProbe,
}
impl<W: AsRawFd> AsRawFd for ProbedWriter<W> {
    fn as_raw_fd(&self) -> RawFd {
        self.gate.calls.fetch_add(1, Ordering::SeqCst);
        if self
            .gate
            .phase
            .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            while self.gate.phase.load(Ordering::SeqCst) != 3 {
                std::hint::spin_loop();
            }
        }
        self.writer.as_raw_fd()
    }
}

#[derive(Debug)]
struct UnretainedWriter(UnixStream);
impl AsRawFd for UnretainedWriter {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

fn probed<W>(writer: W, gate: &Arc<Gate>) -> ProbedWriter<W> {
    ProbedWriter {
        writer,
        gate: gate.clone(),
        _after_writer: DropProbe(gate.clone()),
    }
}

fn delivery(
    retained: bool,
    gate: &Arc<Gate>,
    signals: &[i32],
) -> io::Result<SignalDelivery<UnixStream, SignalOnly>> {
    let (read, writer) = UnixStream::pair()?;
    if retained {
        let writer = SignalWriter {
            writer,
            _receiver: read.try_clone()?,
        };
        SignalDelivery::with_pipe(read, probed(writer, gate), SignalOnly, signals)
    } else {
        SignalDelivery::with_pipe(
            read,
            probed(UnretainedWriter(writer), gate),
            SignalOnly,
            signals,
        )
    }
}

fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !predicate() {
        assert!(Instant::now() < deadline, "lifetime milestone timed out");
        std::thread::yield_now();
    }
}

fn reclaimed(gate: &Gate) {
    assert!(gate.dropped.load(Ordering::SeqCst));
    assert!(!gate.dropped_while_held.load(Ordering::SeqCst));
    let calls = gate.calls.load(Ordering::SeqCst);
    signal_hook::low_level::raise(signal_hook::consts::SIGINT).unwrap();
    assert_eq!(gate.calls.load(Ordering::SeqCst), calls);
    println!("RECLAIMED AND UNREGISTERED");
}

fn close_after_listener(retained: bool) {
    let gate = Arc::new(Gate::default());
    let signals = delivery(retained, &gate, &[signal_hook::consts::SIGINT]).unwrap();
    let handle = signals.handle();
    let (release, start) = mpsc::channel();
    let listener = std::thread::spawn(move || {
        start.recv().unwrap();
        listen(
            OwningSignalIterator::new(signals),
            &CancellationToken::new(),
        );
    });
    gate.arm();
    let closer = handle.clone();
    let closer = std::thread::spawn(move || closer.close());
    gate.entered();
    release.send(()).unwrap();
    listener.join().unwrap();
    println!("LISTENER JOINED BEFORE CLOSE WAKE");
    std::io::stdout().flush().unwrap();
    assert!(!gate.dropped.load(Ordering::SeqCst));
    gate.release();
    closer.join().unwrap();
    drop(handle);
    reclaimed(&gate);
}

fn callback_after_listener(retained: bool) {
    let gate = Arc::new(Gate::default());
    let signals = delivery(retained, &gate, &[signal_hook::consts::SIGINT]).unwrap();
    let handle = signals.handle();
    let listener = std::thread::spawn(move || {
        listen(
            OwningSignalIterator::new(signals),
            &CancellationToken::new(),
        )
    });
    handle.close();
    listener.join().unwrap();
    gate.arm();
    let raising =
        std::thread::spawn(|| signal_hook::low_level::raise(signal_hook::consts::SIGINT).unwrap());
    gate.entered();
    println!("CALLBACK HELD AFTER LISTENER JOIN");
    std::io::stdout().flush().unwrap();
    let dropping = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let (started, finished) = (dropping.clone(), dropped.clone());
    let unregister = std::thread::spawn(move || {
        started.store(true, Ordering::SeqCst);
        drop(handle);
        finished.store(true, Ordering::SeqCst);
    });
    until(|| dropping.load(Ordering::SeqCst));
    assert!(!dropped.load(Ordering::SeqCst));
    assert!(!gate.dropped.load(Ordering::SeqCst));
    gate.release();
    raising.join().unwrap();
    unregister.join().unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    reclaimed(&gate);
}

fn registration_error() {
    let gate = Arc::new(Gate::default());
    let error = delivery(true, &gate, &[signal_hook::consts::SIGINT, 0])
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{error}");
    reclaimed(&gate);
}

fn spawn_error() {
    fn reject(work: impl FnOnce() + Send + 'static) -> io::Result<std::thread::JoinHandle<()>> {
        drop(work);
        Err(io::Error::other("injected spawn failure"))
    }
    let gate = Arc::new(Gate::default());
    let signals = delivery(true, &gate, &[signal_hook::consts::SIGINT]).unwrap();
    let handle = signals.handle();
    let cancelled = CancellationToken::new();
    let error = reject(move || listen(OwningSignalIterator::new(signals), &cancelled)).unwrap_err();
    assert_eq!(error.to_string(), "injected spawn failure");
    assert!(!gate.dropped.load(Ordering::SeqCst));
    drop(handle);
    reclaimed(&gate);
}

#[cfg(target_os = "linux")]
fn descriptor_error(clone: bool) {
    let sockets = clone.then(|| UnixStream::pair().unwrap());
    let mut files = Vec::new();
    while let Ok(file) = std::fs::File::open("/dev/null") {
        files.push(file);
    }
    let error = if let Some((read, _)) = &sockets {
        read.try_clone().unwrap_err()
    } else {
        files.pop();
        UnixStream::pair().unwrap_err()
    };
    assert_eq!(error.raw_os_error(), Some(24), "{error}");
    drop(files);
    drop(sockets);
    let subscription = CtrlC::subscribe().unwrap();
    drop(subscription);
    println!("DESCRIPTOR FAILURE RECOVERED");
}

#[test]
fn signal_writer_retains_receiver_through_close_callbacks_and_error_cleanup() {
    let cases = [
        "close-retained",
        "callback-retained",
        "registration-error",
        "spawn-error",
    ];
    for case in cases {
        supervise(case, false);
    }
    #[cfg(target_os = "linux")]
    for case in [
        "close-unretained",
        "callback-unretained",
        "pair-error",
        "clone-error",
    ] {
        supervise(case, case.ends_with("unretained"));
    }
}

fn supervise(case: &str, negative: bool) {
    use std::os::unix::process::ExitStatusExt;
    let executable = std::env::current_exe().unwrap();
    let mut command = if matches!(case, "pair-error" | "clone-error") {
        let mut command = Command::new("prlimit");
        command.args(["--nofile=64:64", "--"]).arg(&executable);
        command
    } else {
        Command::new(executable)
    };
    let mut child = command
        .args([
            "--exact",
            "cancellation::lifetime_tests::lifetime_child",
            "--nocapture",
        ])
        .env("PLOYZ_SIGNAL_LIFETIME_TEST", case)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).unwrap();
        text
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            child.wait().unwrap();
            let text = reader.join().unwrap();
            panic!("{case}: child exceeded five-second deadline: {text}");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let text = reader.join().unwrap();
    if negative {
        assert_eq!(
            status.signal(),
            Some(signal_hook::consts::SIGPIPE),
            "{case}: {status}: {text}"
        );
    } else {
        assert!(status.success(), "{case}: {status}: {text}");
        assert!(
            text.contains("RECLAIMED AND UNREGISTERED")
                || text.contains("DESCRIPTOR FAILURE RECOVERED"),
            "{case}: {text}"
        );
    }
    if case.starts_with("close-") {
        assert!(text.contains("LISTENER JOINED BEFORE CLOSE WAKE"), "{text}");
    }
    if case.starts_with("callback-") {
        assert!(text.contains("CALLBACK HELD AFTER LISTENER JOIN"), "{text}");
    }
    println!(
        "{case}: exit={:?}, signal={:?}; {text}",
        status.code(),
        status.signal()
    );
}

#[test]
fn lifetime_child() {
    let Ok(case) = std::env::var("PLOYZ_SIGNAL_LIFETIME_TEST") else {
        return;
    };
    sigpipe::reset();
    match case.as_str() {
        "close-retained" => close_after_listener(true),
        "close-unretained" => close_after_listener(false),
        "callback-retained" => callback_after_listener(true),
        "callback-unretained" => callback_after_listener(false),
        "registration-error" => registration_error(),
        "spawn-error" => spawn_error(),
        #[cfg(target_os = "linux")]
        "pair-error" => descriptor_error(false),
        #[cfg(target_os = "linux")]
        "clone-error" => descriptor_error(true),
        _ => panic!("unknown lifetime case {case}"),
    }
}
