//! Throwaway prototype of design.md's output system: one event stream, three renderers.
//! `cargo run -- deploy [--fail] [--json] [--plain]`, `cargo run -- ls [--plain]`.

use std::io::{IsTerminal, Write};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anstream::{eprintln, println};
use anstyle::{AnsiColor, Style};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};

const GOOD: Style = AnsiColor::Green.on_default();
const CHANGE: Style = AnsiColor::Yellow.on_default();
const BAD: Style = AnsiColor::Red.on_default().bold();
const MUTED: Style = Style::new().dimmed();
const NAME: Style = Style::new().bold();
const HINT: Style = AnsiColor::Cyan.on_default().bold();

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Interactive,
    Plain,
    Json,
}

fn mode(args: &[String]) -> Mode {
    if args.iter().any(|a| a == "--json") {
        return Mode::Json;
    }
    let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
    let ci = std::env::var_os("CI").is_some();
    if args.iter().any(|a| a == "--plain") || dumb || ci || !std::io::stderr().is_terminal() {
        return Mode::Plain;
    }
    Mode::Interactive
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Pending,
    Pulling,
    Starting,
    Healthy,
    Unchanged,
    Unhealthy,
}

impl State {
    fn word(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Pulling => "pulling",
            State::Starting => "starting",
            State::Healthy => "healthy",
            State::Unchanged => "unchanged",
            State::Unhealthy => "unhealthy",
        }
    }
    fn finished(self) -> bool {
        matches!(self, State::Healthy | State::Unchanged | State::Unhealthy)
    }
}

struct Task {
    service: &'static str,
    server: &'static str,
}

/// What the daemon would stream: (ms since start, task index, new state).
fn script(fail: bool) -> (Vec<Task>, Vec<(u64, usize, State)>) {
    let tasks = vec![
        Task { service: "web", server: "alpha" },
        Task { service: "web", server: "beta" },
        Task { service: "api", server: "alpha" },
        Task { service: "worker", server: "beta" },
        Task { service: "postgres", server: "gamma" },
    ];
    let mut events = vec![
        (300, 2, State::Unchanged),
        (400, 4, State::Unchanged),
        (500, 0, State::Pulling),
        (600, 1, State::Pulling),
        (700, 3, State::Pulling),
        (1800, 0, State::Starting),
        (2100, 1, State::Starting),
        (2300, 3, State::Starting),
        (3600, 0, State::Healthy),
        (4300, 1, State::Healthy),
    ];
    events.push(if fail { (6200, 3, State::Unhealthy) } else { (5200, 3, State::Healthy) });
    (tasks, events)
}

fn label(t: &Task, width: usize) -> String {
    let plain = format!("{} on {}", t.service, t.server);
    let pad = " ".repeat(width.saturating_sub(plain.len()));
    format!("{}{MUTED} on {MUTED:#}{}{pad}", t.service, t.server)
}

fn tone(s: State) -> Style {
    match s {
        State::Healthy => GOOD,
        State::Unhealthy => BAD,
        State::Unchanged => MUTED,
        _ => CHANGE,
    }
}

fn deploy(mode: Mode, fail: bool) -> i32 {
    let (tasks, events) = script(fail);
    let width = tasks.iter().map(|t| t.service.len() + 4 + t.server.len()).max().unwrap_or(0);
    let title = format!("Deploying {NAME}#37{NAME:#} of {NAME}shop/production{NAME:#} to 3 servers");
    // indicatif writes these strings itself, so anstream's NO_COLOR handling never sees them.
    let color = anstream::AutoStream::choice(&std::io::stderr()) != anstream::ColorChoice::Never;
    let paint = move |s: String| if color { s } else { anstream::adapter::strip_str(&s).to_string() };

    let multi = MultiProgress::with_draw_target(match mode {
        Mode::Interactive => ProgressDrawTarget::stderr(),
        _ => ProgressDrawTarget::hidden(),
    });
    let header = multi.add(ProgressBar::new(tasks.len() as u64));
    header.set_style(ProgressStyle::with_template("{msg}  {pos}/{len}").unwrap());
    header.set_message(paint(title.clone()));
    let row_style = ProgressStyle::with_template("{prefix} {msg} {secs}")
        .unwrap()
        .with_key("secs", move |s: &ProgressState, w: &mut dyn std::fmt::Write| {
            let _ = write!(w, "{}", paint(format!("{MUTED}{:>5.1}s{MUTED:#}", s.elapsed().as_secs_f64())));
        });
    let rows: Vec<ProgressBar> = tasks
        .iter()
        .map(|t| {
            let bar = multi.add(ProgressBar::new_spinner());
            bar.set_style(row_style.clone());
            bar.set_prefix(paint(format!("{MUTED}·{MUTED:#}")));
            bar.set_message(paint(format!("{}  {MUTED}{:<9}{MUTED:#}", label(t, width), "pending")));
            bar
        })
        .collect();
    if mode == Mode::Interactive {
        header.enable_steady_tick(Duration::from_millis(100));
    } else {
        eprintln!("{title}");
    }

    let start = Instant::now();
    let mut states = vec![State::Pending; tasks.len()];
    let mut last_line = Instant::now();
    let mut events = events.into_iter().peekable();
    while let Some(&(at, i, state)) = events.peek() {
        let now = start.elapsed().as_millis() as u64;
        if now < at {
            if mode != Mode::Interactive && last_line.elapsed() >= Duration::from_secs(3) {
                // The real heartbeat is 30s; 3s keeps the demo short.
                let waiting: Vec<String> = (0..tasks.len())
                    .filter(|&j| !states[j].finished())
                    .map(|j| format!("{} on {}", tasks[j].service, tasks[j].server))
                    .collect();
                eprintln!("  {MUTED}still waiting: {}{MUTED:#}", waiting.join(", "));
                last_line = Instant::now();
            }
            sleep(Duration::from_millis(20));
            continue;
        }
        events.next();
        if states[i] == State::Pending {
            rows[i].reset_elapsed();
            if mode == Mode::Interactive {
                rows[i].enable_steady_tick(Duration::from_millis(100));
                rows[i].set_style(row_style.clone().template("{spinner:.yellow} {msg} {secs}").unwrap()
                    .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "));
            }
        }
        states[i] = state;
        let t = &tasks[i];
        let secs = rows[i].elapsed().as_secs_f64();
        let word = format!("{}{}{:#}", tone(state), state.word(), tone(state));
        let pad = " ".repeat(9 - state.word().len());
        rows[i].set_message(paint(format!("{}  {word}{pad}", label(t, width))));
        if state.finished() {
            let mark = match state {
                State::Unhealthy => format!("{BAD}✘{BAD:#}"),
                State::Unchanged => format!("{MUTED}✔{MUTED:#}"),
                _ => format!("{GOOD}✔{GOOD:#}"),
            };
            rows[i].set_style(row_style.clone().template(if state == State::Unchanged {
                "{prefix} {msg}"
            } else {
                "{prefix} {msg} {secs}"
            }).unwrap());
            rows[i].set_prefix(paint(mark));
            rows[i].finish();
            header.inc(1);
        }
        if mode != Mode::Interactive {
            let timing = if state.finished() && state != State::Unchanged {
                format!(" {MUTED}({secs:.1}s){MUTED:#}")
            } else {
                String::new()
            };
            eprintln!("  {}{MUTED}:{MUTED:#} {word}{timing}", label(t, 0));
            last_line = Instant::now();
        }
    }
    header.finish();

    let count = |s: State| states.iter().filter(|&&x| x == s).count();
    let mut parts = vec![format!("{GOOD}{} updated{GOOD:#}", count(State::Healthy))];
    parts.push(format!("{} unchanged", count(State::Unchanged)));
    if count(State::Unhealthy) > 0 {
        parts.push(format!("{BAD}{} failed{BAD:#}", count(State::Unhealthy)));
    }
    let summary = format!("{} {MUTED}·{MUTED:#} across 3 servers", parts.join(&format!(" {MUTED}·{MUTED:#} ")));
    let visible = anstream::adapter::strip_str(&summary).to_string().chars().count();
    eprintln!("{MUTED}{}{MUTED:#}", "─".repeat(visible));
    eprintln!("{summary}");

    let task_json = tasks
        .iter()
        .zip(&states)
        .map(|(t, s)| format!(r#"{{"service":"{}","server":"{}","outcome":"{}"}}"#, t.service, t.server, s.word()))
        .collect::<Vec<_>>()
        .join(",");
    if fail {
        eprintln!();
        eprintln!("{BAD}error:{BAD:#} Deployment {NAME}#37{NAME:#} failed: worker on beta is unhealthy.");
        eprintln!("  {BAD}cause:{BAD:#} container exited with code 1 after 3 restarts");
        eprintln!("{MUTED}Last 3 log lines from worker on beta:{MUTED:#}");
        eprintln!("  {MUTED}│{MUTED:#} booting worker v1.5");
        eprintln!("  {MUTED}│{MUTED:#} connecting to postgres.shop.internal:5432");
        eprintln!("  {MUTED}│{MUTED:#} panic: missing DATABASE_URL");
        eprintln!("{HINT}inspect:{HINT:#} ployz logs worker --machine beta");
        eprintln!("{HINT}retry:{HINT:#} ployz deploy --project shop");
        if mode == Mode::Json {
            println!(r#"{{"error":{{"code":"deploy_failed","message":"Deployment #37 failed: worker on beta is unhealthy.","cause":["container exited with code 1 after 3 restarts"],"details":{{"deployment":37,"project":"shop","env":"production","tasks":[{task_json}],"logs":{{"service":"worker","machine":"beta","lines":["booting worker v1.5","connecting to postgres.shop.internal:5432","panic: missing DATABASE_URL"]}},"inspect":["ployz logs worker --machine beta"],"retry":["ployz deploy --project shop"]}}}}}}"#);
        }
        return 1;
    }
    match mode {
        Mode::Json => println!(
            r#"{{"deployment":37,"project":"shop","env":"production","status":"applied","updated":{},"unchanged":{},"failed":0,"servers":3,"tasks":[{task_json}]}}"#,
            count(State::Healthy),
            count(State::Unchanged),
        ),
        _ => println!("Deployed {NAME}#37{NAME:#} to {NAME}shop/production{NAME:#}."),
    }
    0
}

fn ls(mode: Mode) {
    // Tables follow stdout, not stderr: `ployz service ls | cut -f1` must get TSV.
    let mode = if mode == Mode::Interactive && !std::io::stdout().is_terminal() { Mode::Plain } else { mode };
    let rows = [
        ("web", "acme/web:1.5", "2/2", State::Healthy, "https://web.acme.com"),
        ("api", "acme/api:1.11", "1/1", State::Healthy, "-"),
        ("worker", "acme/worker:1.5", "0/1", State::Unhealthy, "-"),
        ("postgres", "postgres:17", "1/1", State::Unchanged, "-"),
    ];
    let header = ["NAME", "IMAGE", "READY", "STATUS", "ADDRESS"];
    let status = |s: State| match s {
        State::Healthy => "running",
        State::Unhealthy => "crashed",
        _ => "running",
    };
    match mode {
        Mode::Json => println!(
            r#"{{"services":[{}]}}"#,
            rows.iter()
                .map(|r| {
                    let address = if r.4 == "-" { "null".to_string() } else { format!(r#""{}""#, r.4) };
                    format!(r#"{{"name":"{}","image":"{}","ready":"{}","status":"{}","address":{address}}}"#, r.0, r.1, r.2, status(r.3))
                })
                .collect::<Vec<_>>()
                .join(",")
        ),
        Mode::Plain => {
            println!("{}", header.join("\t"));
            for r in rows {
                println!("{}\t{}\t{}\t{}\t{}", r.0, r.1, r.2, status(r.3), r.4);
            }
        }
        Mode::Interactive => {
            let w = [8, 15, 5, 9];
            println!("{NAME}{:<w0$}   {:<w1$}   {:<w2$}   {:<w3$}   {}{NAME:#}", header[0], header[1], header[2], header[3], header[4],
                w0 = w[0], w1 = w[1], w2 = w[2], w3 = w[3]);
            for r in rows {
                let (mark, t) = match r.3 {
                    State::Unhealthy => ("✘", BAD),
                    _ => ("✔", GOOD),
                };
                println!("{:<8}   {:<15}   {:<5}   {t}{mark} {:<7}{t:#}   {}", r.0, r.1, r.2, status(r.3), r.4);
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = mode(&args);
    if mode != Mode::Interactive {
        anstream::ColorChoice::Never.write_global();
    }
    let code = match args.first().map(String::as_str) {
        Some("ls") => {
            ls(mode);
            0
        }
        _ => deploy(mode, args.iter().any(|a| a == "--fail")),
    };
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}
