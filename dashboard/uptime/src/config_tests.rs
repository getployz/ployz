use std::collections::HashMap;

use super::{ConfigError, Mode, Secret};

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn parse(mode: &str, vars: &HashMap<String, String>) -> Result<Mode, ConfigError> {
    let args = vec!["ployz-uptime".to_owned(), mode.to_owned()];
    Mode::from(&args, &|k| vars.get(k).cloned())
}

const CONTROL: &[(&str, &str)] = &[
    ("DATABASE_URL", "postgres://u:p@db/x"),
    ("UPTIME_REGIONS", "ams, fra,ams"),
    ("UPTIME_AGENT_TOKEN", "hunter2"),
];

type Case = (
    &'static str,
    &'static [(&'static str, &'static str)],
    ConfigError,
);

const AGENT: &[(&str, &str)] = &[
    ("UPTIME_CONTROL_URL", "http://control.railway.internal:8080"),
    ("UPTIME_AGENT_TOKEN", "hunter2"),
    ("RAILWAY_REPLICA_REGION", "fra"),
    ("RAILWAY_REPLICA_ID", "r-1"),
];

#[test]
fn every_missing_or_bad_variable_is_named_without_its_value() {
    let cases: &[Case] = &[
        ("control", &[], ConfigError::Missing("DATABASE_URL")),
        (
            "control",
            &[("DATABASE_URL", "postgres://x")],
            ConfigError::Missing("UPTIME_REGIONS"),
        ),
        (
            "control",
            &[("DATABASE_URL", "postgres://x"), ("UPTIME_REGIONS", " , ")],
            ConfigError::Invalid {
                var: "UPTIME_REGIONS",
                why: "names no region".to_owned(),
            },
        ),
        (
            "control",
            &[("DATABASE_URL", "postgres://x"), ("UPTIME_REGIONS", "fra")],
            ConfigError::Missing("UPTIME_AGENT_TOKEN"),
        ),
        ("agent", &[], ConfigError::Missing("UPTIME_CONTROL_URL")),
        (
            "agent",
            &[("UPTIME_CONTROL_URL", "https://control")],
            ConfigError::Invalid {
                var: "UPTIME_CONTROL_URL",
                why: "must be http://: the link runs over private networking, which is plain HTTP"
                    .to_owned(),
            },
        ),
        (
            "agent",
            &[
                ("UPTIME_CONTROL_URL", "http://control"),
                ("UPTIME_AGENT_TOKEN", "t"),
            ],
            ConfigError::Missing("REGION"),
        ),
        ("all", &[], ConfigError::Missing("DATABASE_URL")),
        ("serve", CONTROL, ConfigError::Usage),
    ];
    for (mode, vars, want) in cases {
        let got = parse(mode, &env(vars)).err();
        assert_eq!(got.as_ref(), Some(want), "mode {mode} with {vars:?}");
        let text = got.map(|e| e.to_string()).unwrap_or_default();
        assert!(!text.contains("postgres://"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
    }
}

#[test]
fn control_dedupes_regions_keeps_the_token_and_listens_on_port_by_default() {
    let Ok(Mode::Control(control)) = parse("control", &env(CONTROL)) else {
        panic!("control mode expected");
    };
    let names: Vec<&str> = control.regions.iter().map(|r| r.as_str()).collect();
    assert_eq!(names, ["ams", "fra"]);
    assert_eq!(
        control.listen.to_string(),
        "[::]:8080",
        "dual-stack for private networking"
    );
    assert!(
        control
            .agent_token
            .as_ref()
            .is_some_and(|t| t.matches(b"hunter2"))
    );
    assert_eq!(control.database_url.expose(), "postgres://u:p@db/x");
}

#[test]
fn a_bad_port_or_region_name_is_refused() {
    let mut vars = env(CONTROL);
    vars.insert("PORT".to_owned(), "eighty".to_owned());
    assert!(matches!(
        parse("control", &vars),
        Err(ConfigError::Invalid { var: "PORT", .. })
    ));
    let mut vars = env(CONTROL);
    vars.insert("UPTIME_REGIONS".to_owned(), "fra,Not A Region!".to_owned());
    assert!(matches!(
        parse("control", &vars),
        Err(ConfigError::Invalid {
            var: "UPTIME_REGIONS",
            ..
        })
    ));
}

#[test]
fn agent_names_itself_after_region_and_replica() {
    let Ok(Mode::Agent(agent)) = parse("agent", &env(AGENT)) else {
        panic!("agent mode expected");
    };
    assert_eq!(agent.region.as_str(), "fra");
    assert_eq!(&*agent.agent.0, "fra/r-1");
    assert!(agent.token.as_ref().is_some_and(|t| t.matches(b"hunter2")));
    assert!(!agent.probe.allow_ipv6);
    let mut vars = env(AGENT);
    vars.insert("UPTIME_IPV6".to_owned(), "1".to_owned());
    vars.insert("REGION".to_owned(), "ams".to_owned());
    let Ok(Mode::Agent(agent)) = parse("agent", &vars) else {
        panic!("agent mode expected");
    };
    assert!(agent.probe.allow_ipv6);
    assert_eq!(agent.region.as_str(), "ams");
}

#[test]
fn all_defaults_to_one_local_region_with_no_token() {
    let vars = env(&[("DATABASE_URL", "postgres://x"), ("PORT", "9000")]);
    let Ok(Mode::All(control, agent)) = parse("all", &vars) else {
        panic!("all mode expected");
    };
    assert_eq!(control.regions.len(), 1);
    assert_eq!(control.regions.first().map(|r| r.as_str()), Some("local"));
    assert!(control.agent_token.is_none());
    assert!(agent.token.is_none());
    assert_eq!(agent.control_url, "http://[::]:9000");
    assert_eq!(agent.region.as_str(), "local");
}

#[test]
fn a_secret_never_shows_its_value() {
    let secret = Secret::new("hunter2");
    assert_eq!(format!("{secret:?}"), "[redacted]");
    assert_eq!(secret.to_string(), "[redacted]");
    assert!(secret.matches(b"hunter2"));
    assert!(!secret.matches(b"hunter3"));
    assert!(!secret.matches(b"hunter"));
}
