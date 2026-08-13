use std::{path::Path, process::Command};

use chrono::{DateTime, Local, TimeZone as _, Utc};
use chrono_tz::Tz;
use oci_spec::distribution::Reference;

use super::{convert::invalid, model::ComposeError};

#[derive(Clone, Debug, Default)]
pub(super) struct ImageState {
    sha: String,
    commit_time: Option<DateTime<Utc>>,
    dirty: bool,
}

type RenderStop<'a> = Option<(&'a str, &'a str)>;

impl ImageState {
    pub(super) fn inspect(directory: &Path) -> Result<Self, ComposeError> {
        let Some(sha) = git(directory, &["rev-parse", "--verify", "HEAD"])? else {
            return Ok(Self::default());
        };
        let timestamp = git(directory, &["log", "-1", "--format=%ct"])?
            .ok_or_else(|| invalid("git repository has no commit timestamp"))?
            .parse::<i64>()
            .map_err(invalid)?;
        let status = git(directory, &["status", "--porcelain"])?
            .ok_or_else(|| invalid("git repository has no status"))?;
        Ok(Self {
            sha,
            commit_time: DateTime::from_timestamp(timestamp, 0),
            dirty: !status.is_empty(),
        })
    }

    pub(super) fn image(
        &self,
        project: &str,
        service: &str,
        image: Option<&str>,
        build: bool,
    ) -> Result<String, ComposeError> {
        let mut image = image
            .map(str::to_owned)
            .or_else(|| build.then(|| format!("{project}/{service}:{{{{.Tag}}}}")))
            .unwrap_or_default();
        image = self.render(&image, project, service)?;
        if build && !image.is_empty() && !has_tag(&image)? {
            image.push(':');
            image.push_str(&self.tag()?);
        }
        Ok(image)
    }

    fn tag(&self) -> Result<String, ComposeError> {
        if let Some(commit_time) = self.commit_time {
            Ok(format!(
                "{}.{}{}",
                commit_time.format("%Y-%m-%d-%H%M%S"),
                self.short_sha(7),
                if self.dirty { ".dirty" } else { "" }
            ))
        } else {
            Ok(Utc::now().format("%Y-%m-%d-%H%M%S").to_string())
        }
    }

    fn render(&self, template: &str, project: &str, service: &str) -> Result<String, ComposeError> {
        let (rendered, stop) = self.render_until(template, project, service)?;
        if stop.is_some() {
            return Err(invalid("unexpected template block terminator"));
        }
        Ok(rendered)
    }

    fn render_until<'a>(
        &self,
        mut template: &'a str,
        project: &str,
        service: &str,
    ) -> Result<(String, RenderStop<'a>), ComposeError> {
        let mut rendered = String::new();
        while let Some(start) = template.find("{{") {
            rendered.push_str(&template[..start]);
            let expression = &template[start + 2..];
            let end = expression
                .find("}}")
                .ok_or_else(|| invalid(format!("unclosed image template '{template}'")))?;
            let token = expression[..end].trim();
            template = &expression[end + 2..];
            if matches!(token, "else" | "end") {
                return Ok((rendered, Some((token, template))));
            }
            if let Some(condition) = token.strip_prefix("if ") {
                let enabled = match condition {
                    ".Git.IsRepo" => self.commit_time.is_some(),
                    ".Git.IsDirty" => self.dirty,
                    _ => {
                        return Err(invalid(format!(
                            "unsupported image condition '{condition}'"
                        )));
                    }
                };
                let (yes, stop) = self.render_until(template, project, service)?;
                let Some((stop, rest)) = stop else {
                    return Err(invalid("unterminated image template condition"));
                };
                let (no, rest) = if stop == "else" {
                    let (no, stop) = self.render_until(rest, project, service)?;
                    let Some(("end", rest)) = stop else {
                        return Err(invalid("unterminated image template condition"));
                    };
                    (no, rest)
                } else {
                    (String::new(), rest)
                };
                rendered.push_str(if enabled { &yes } else { &no });
                template = rest;
            } else {
                rendered.push_str(&self.expression(token, project, service)?);
            }
        }
        rendered.push_str(template);
        Ok((rendered, None))
    }

    fn expression(
        &self,
        expression: &str,
        project: &str,
        service: &str,
    ) -> Result<String, ComposeError> {
        match expression {
            ".Project" => Ok(project.into()),
            ".Service" => Ok(service.into()),
            ".Tag" => self.tag(),
            ".Git.SHA" => Ok(self.sha.clone()),
            "gitsha" => Ok(self.sha.clone()),
            _ => {
                let args = shell_words::split(expression).map_err(invalid)?;
                if matches!(args.first().map(String::as_str), Some("date" | "gitdate"))
                    && !expression.contains('"')
                {
                    return Err(invalid("image date formats must be quoted"));
                }
                match args.as_slice() {
                    [function, length] if function == "gitsha" => {
                        let length = length.parse::<isize>().map_err(invalid)?;
                        Ok(if length <= 0 {
                            self.sha.clone()
                        } else {
                            self.short_sha(length as usize)
                        })
                    }
                    [function, format] if function == "gitdate" => Ok(self
                        .commit_time
                        .map(|date| format_date(date, format, None))
                        .transpose()?
                        .unwrap_or_default()),
                    [function, format, timezone] if function == "gitdate" => Ok(self
                        .commit_time
                        .map(|date| format_date(date, format, Some(timezone)))
                        .transpose()?
                        .unwrap_or_default()),
                    [function, format] if function == "date" => {
                        format_date(Utc::now(), format, None)
                    }
                    [function, format, timezone] if function == "date" => {
                        format_date(Utc::now(), format, Some(timezone))
                    }
                    _ => Err(invalid(format!(
                        "unsupported image template expression '{{{{{expression}}}}}'"
                    ))),
                }
            }
        }
    }

    fn short_sha(&self, length: usize) -> String {
        self.sha.chars().take(length).collect()
    }
}

fn git(directory: &Path, args: &[&str]) -> Result<Option<String>, ComposeError> {
    let output = match Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(invalid(error)),
    };
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
    ))
}

fn format_date(
    date: DateTime<Utc>,
    format: &str,
    timezone: Option<&str>,
) -> Result<String, ComposeError> {
    let format = go_time_format(format);
    match timezone {
        None | Some("UTC") => Ok(date.format(&format).to_string()),
        Some("Local") => Ok(date.with_timezone(&Local).format(&format).to_string()),
        Some(timezone) => {
            let timezone = timezone.parse::<Tz>().map_err(invalid)?;
            Ok(timezone
                .from_utc_datetime(&date.naive_utc())
                .format(&format)
                .to_string())
        }
    }
}

fn go_time_format(format: &str) -> String {
    format
        .replace("2006", "%Y")
        .replace("01", "%m")
        .replace("02", "%d")
        .replace("15", "%H")
        .replace("04", "%M")
        .replace("05", "%S")
        .replace("MST", "%Z")
        .replace("-0700", "%z")
}

fn has_tag(image: &str) -> Result<bool, ComposeError> {
    let reference = image
        .parse::<Reference>()
        .map_err(|error| invalid(format!("parse image reference '{image}': {error}")))?;
    Ok(reference.digest().is_some()
        || image
            .rsplit('/')
            .next()
            .is_some_and(|component| component.contains(':')))
}
