//! Process configuration from the command line and the environment. Every error names the
//! variable and never its value, so a bad `DATABASE_URL` or token is reported without leaking.

use std::fmt;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;

use crate::agent::probe::ProbeConfig;
use crate::agent::run::AgentRun;
use crate::domain::{AgentName, Region, RegionName};

/// A value that must never appear in logs, errors or debug output. Only [`Secret::expose`] reads
/// it, at the one place it is used.
#[derive(Clone)]
pub struct Secret(Arc<str>);

impl Secret {
    #[must_use]
    pub fn new(value: &str) -> Secret {
        Secret(Arc::from(value))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Constant-time equality for presented credentials. Length is compared first and leaks only
    /// itself; the bytes are folded with XOR so no early exit depends on where they differ.
    #[must_use]
    pub fn matches(&self, presented: &[u8]) -> bool {
        let own = self.0.as_bytes();
        if own.len() != presented.len() {
            return false;
        }
        own.iter()
            .zip(presented)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Debug)]
pub enum Mode {
    Control(ControlConfig),
    Agent(AgentConfig),
    All(ControlConfig, AgentConfig),
}

#[derive(Debug)]
pub struct ControlConfig {
    pub database_url: Secret,
    /// In `UPTIME_REGIONS` order, each code once.
    pub regions: Vec<RegionConfig>,
    pub agent_token: Option<Secret>,
    pub listen: SocketAddr,
}

/// One `UPTIME_REGIONS` entry: `code` or `code=Name`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegionConfig {
    pub region: Region,
    pub name: Option<RegionName>,
}

impl ControlConfig {
    #[must_use]
    pub fn region_codes(&self) -> Vec<Region> {
        self.regions.iter().map(|r| r.region.clone()).collect()
    }
}

#[derive(Debug)]
pub struct AgentConfig {
    pub control_url: String,
    pub token: Option<Secret>,
    pub region: Region,
    pub agent: AgentName,
    pub probe: ProbeConfig,
    pub run: AgentRun,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("Usage: ployz-uptime <control|agent|all>")]
    Usage,
    #[error("Set {0}.")]
    Missing(&'static str),
    #[error("{var} {why}.")]
    Invalid { var: &'static str, why: String },
}

const DEFAULT_PORT: u16 = 8080;
const DEFAULT_REGION: &str = "local";

impl Mode {
    /// # Errors
    /// No or an unknown mode argument, a required variable missing, or a variable the mode can't
    /// use as given. The message names the variable, never its value.
    pub fn from(
        args: &[String],
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Mode, ConfigError> {
        let mode = args.get(1).map(String::as_str);
        match mode {
            Some("control") => Ok(Mode::Control(ControlConfig::from(env, true)?)),
            Some("agent") => Ok(Mode::Agent(AgentConfig::from(env)?)),
            Some("all") => {
                let control = ControlConfig::from(env, false)?;
                let Some(region) = control.regions.first().map(|r| r.region.clone()) else {
                    return Err(invalid("UPTIME_REGIONS", "names no region"));
                };
                let probe = probe_config(env);
                let agent = AgentConfig {
                    control_url: format!("http://{}", control.listen),
                    token: None,
                    region: region.clone(),
                    agent: AgentName(region.as_str().into()),
                    run: AgentRun::production(&probe),
                    probe,
                };
                Ok(Mode::All(control, agent))
            }
            Some(_) | None => Err(ConfigError::Usage),
        }
    }
}

impl ControlConfig {
    fn from(
        env: &dyn Fn(&str) -> Option<String>,
        remote_agents: bool,
    ) -> Result<Self, ConfigError> {
        let database_url = required(env, "DATABASE_URL")?;
        let regions = match env("UPTIME_REGIONS") {
            Some(list) => regions(&list)?,
            None if remote_agents => return Err(ConfigError::Missing("UPTIME_REGIONS")),
            None => regions(DEFAULT_REGION)?,
        };
        let agent_token = if remote_agents {
            Some(required(env, "UPTIME_AGENT_TOKEN")?)
        } else {
            None
        };
        Ok(ControlConfig {
            database_url,
            regions,
            agent_token,
            listen: SocketAddr::from((Ipv6Addr::UNSPECIFIED, port(env)?)),
        })
    }
}

impl AgentConfig {
    fn from(env: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let control_url = env("UPTIME_CONTROL_URL")
            .filter(|v| !v.trim().is_empty())
            .ok_or(ConfigError::Missing("UPTIME_CONTROL_URL"))?;
        if control_url.starts_with("https://") {
            return Err(invalid(
                "UPTIME_CONTROL_URL",
                "must be http://: the link runs over private networking, which is plain HTTP",
            ));
        }
        if !control_url.starts_with("http://") {
            return Err(invalid("UPTIME_CONTROL_URL", "must start with http://"));
        }
        let token = required(env, "UPTIME_AGENT_TOKEN")?;
        let region_name = env("REGION")
            .or_else(|| env("RAILWAY_REPLICA_REGION"))
            .filter(|v| !v.trim().is_empty())
            .ok_or(ConfigError::Missing("REGION"))?;
        let region = Region::parse(region_name.trim())
            .map_err(|e| invalid("REGION", &format!("is not a region name: {e}")))?;
        let agent = match env("RAILWAY_REPLICA_ID").filter(|v| !v.trim().is_empty()) {
            Some(replica) => format!("{}/{}", region.as_str(), replica.trim()),
            None => region.as_str().to_owned(),
        };
        let probe = probe_config(env);
        Ok(AgentConfig {
            control_url,
            token: Some(token),
            region,
            agent: AgentName(agent.into()),
            run: AgentRun::production(&probe),
            probe,
        })
    }
}

fn probe_config(env: &dyn Fn(&str) -> Option<String>) -> ProbeConfig {
    ProbeConfig {
        allow_ipv6: env("UPTIME_IPV6").is_some_and(|v| v.trim() == "1"),
        ..ProbeConfig::default()
    }
}

fn required(
    env: &dyn Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<Secret, ConfigError> {
    env(var)
        .filter(|v| !v.trim().is_empty())
        .map(|v| Secret::new(v.trim()))
        .ok_or(ConfigError::Missing(var))
}

fn port(env: &dyn Fn(&str) -> Option<String>) -> Result<u16, ConfigError> {
    match env("PORT") {
        None => Ok(DEFAULT_PORT),
        Some(raw) => raw
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(|| invalid("PORT", "is not a port number")),
    }
}

/// A comma list of `code` or `code=Name`, trimmed. A code listed twice is refused, since its two
/// entries could name it differently.
fn regions(list: &str) -> Result<Vec<RegionConfig>, ConfigError> {
    let mut out: Vec<RegionConfig> = Vec::new();
    for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (code, name) = match entry.split_once('=') {
            Some((code, name)) => (code.trim(), Some(name)),
            None => (entry, None),
        };
        let region = Region::parse(code)
            .map_err(|e| invalid("UPTIME_REGIONS", &format!("holds a bad region name: {e}")))?;
        let name = name
            .map(RegionName::parse)
            .transpose()
            .map_err(|e| invalid("UPTIME_REGIONS", &format!("holds a bad display name: {e}")))?;
        if out.iter().any(|r| r.region == region) {
            return Err(invalid(
                "UPTIME_REGIONS",
                &format!("lists {} twice", region.as_str()),
            ));
        }
        out.push(RegionConfig { region, name });
    }
    if out.is_empty() {
        return Err(invalid("UPTIME_REGIONS", "names no region"));
    }
    Ok(out)
}

fn invalid(var: &'static str, why: &str) -> ConfigError {
    ConfigError::Invalid {
        var,
        why: why.to_owned(),
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
