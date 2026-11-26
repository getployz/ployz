//! Store values and staged changes as people read them.

use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

/// A unit enum variant as a person reads it: its JSON word with spaces, such as `not applied` for `not_applied`.
pub(crate) fn word(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(|word| word.replace('_', " ")))
        .unwrap_or_default()
}

/// A value as a change line shows it: a long one cut to the start that fits 80 columns, and its length, so one big variable doesn't fill the terminal.
/// URL user info is masked before the cut, which could drop the `@` that marks it.
pub(crate) fn shown(value: &serde_json::Value) -> String {
    const SHOWN: usize = 80;
    let raw = value.to_string();
    let (start, whole) = fitted(&crate::ui::mask(&raw), SHOWN);
    if whole {
        return start;
    }
    let length = value
        .as_str()
        .map_or_else(|| raw.chars().count(), |raw| raw.chars().count());
    format!("{start}… ({length} chars)")
}

/// JSON `text` as shown, kept piece by piece while it fits `columns`, and whether all of it did.
fn fitted(text: &str, columns: usize) -> (String, bool) {
    let mut start = String::new();
    let mut width = 0;
    for piece in pieces(text) {
        width += piece.width();
        if width > columns {
            return (start, false);
        }
        start.push_str(&piece);
    }
    (start, true)
}

/// JSON `text` as shown, in pieces a cut must not split: a JSON escape such as `\n` or
/// `\u0001`, each [`crate::ui::unprintable`] character as its `\u{…}` escape, or one grapheme
/// cluster, measured whole since its marks and selectors change its width.
fn pieces(text: &str) -> impl Iterator<Item = String> + '_ {
    let mut rest = text;
    std::iter::from_fn(move || {
        let c = rest.chars().next()?;
        let (piece, length) = if c == '\\' {
            let length = if rest.get(1..2) == Some("u") { 6 } else { 2 };
            let escape = rest.get(..length).unwrap_or(rest);
            (escape.to_owned(), escape.len())
        } else if crate::ui::unprintable(c) {
            (crate::ui::escaped(c.encode_utf8(&mut [0; 4])), c.len_utf8())
        } else {
            let printable = rest
                .find(|c| c == '\\' || crate::ui::unprintable(c))
                .map_or(rest, |end| rest.get(..end).unwrap_or(rest));
            let grapheme = printable.graphemes(true).next().unwrap_or(printable);
            (grapheme.to_owned(), grapheme.len())
        };
        rest = rest.get(length..).unwrap_or_default();
        Some(piece)
    })
}

/// A staged Setting change as lines, `web.replicas: 1 → 3`. A domain change reads as
/// one line per domain, `web.domains: added shop.acme.ployz.app`, never as the field storing it.
pub(crate) fn changed(
    row: &ployz_core::config::ServiceSettingChange,
    rows: &[ployz_core::config::ServiceSettingChange],
) -> Vec<String> {
    if let Some(lines) = config_changed(row).or_else(|| source_changed(row)) {
        return lines;
    }
    let Some(domains) = ployz_store::DomainChange::of(row) else {
        let value = |value: &serde_json::Value| match (value.as_f64(), value.as_object()) {
            (Some(gb), _) if row.path.ends_with(".memLimit") => {
                ployz_core::memory_size::memory_size(gb)
            }
            (Some(1.0), _) if row.path.ends_with(".cpuLimit") => "1 vCPU".to_owned(),
            (Some(cpus), _) if row.path.ends_with(".cpuLimit") => format!("{cpus} vCPUs"),
            (_, Some(counts)) if row.path.ends_with(".replicas") => counts
                .iter()
                .map(|(region, count)| format!("{} {count}", crate::ui::escaped(region)))
                .collect::<Vec<_>>()
                .join(" · "),
            _ => shown(value),
        };
        return vec![format!(
            "{}: {} → {}",
            crate::ui::escaped(&row.shown_path(rows)),
            value(&row.before),
            value(&row.after)
        )];
    };
    let service = row.path.split('.').next().unwrap_or_default();
    domains
        .iter()
        .map(|domain| format!("{service}.domains: {domain}"))
        .collect()
}

/// Each part of a source row and the Setting `set` and `get` name it by.
const SOURCE_PARTS: [(&str, &str); 4] = [
    ("image", "image"),
    ("credentials", "registryCredential"),
    ("repository", "repository"),
    ("rootDir", "rootDir"),
];

/// A source row as the Settings it holds, `web.image: "nginx:1.26" → "nginx:1.27"`,
/// or `None` when no part `set` names changed.
fn source_changed(row: &ployz_core::config::ServiceSettingChange) -> Option<Vec<String>> {
    let service = crate::ui::escaped(row.path.strip_suffix(".source")?);
    // No registry credential reads as unset, so a removed Service doesn't list one.
    let part = |value: &serde_json::Value, field: &str| {
        value
            .get(field)
            .filter(|part| **part != serde_json::Value::Bool(false))
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    let lines: Vec<String> = SOURCE_PARTS
        .iter()
        .filter_map(|&(field, setting)| {
            let (before, after) = (part(&row.before, field), part(&row.after, field));
            (before != after).then(|| {
                format!(
                    "{service}.{setting}: {} → {}",
                    shown(&before),
                    shown(&after)
                )
            })
        })
        .collect();
    (!lines.is_empty()).then_some(lines)
}

/// A Config row by name, `name: "app" → "app2"` or `files: removed empty.txt`, never
/// by the id its path keeps.
fn config_changed(row: &ployz_core::config::ServiceSettingChange) -> Option<Vec<String>> {
    let (_, field) = row.path.strip_prefix("configs.@")?.split_once('.')?;
    Some(vec![config_field_changed(field, &row.before, &row.after)])
}

/// A change to `field` of a Config, as [`changed`] reads a Config row.
pub(crate) fn config_field_changed(
    field: &str,
    before: &serde_json::Value,
    after: &serde_json::Value,
) -> String {
    match field.strip_prefix("files.") {
        Some(file) => {
            let edit = match (before.is_null(), after.is_null()) {
                (true, _) => "added",
                (_, true) => "removed",
                _ => "changed",
            };
            format!("files: {edit} {}", crate::ui::escaped(file))
        }
        None => format!(
            "{}: {} → {}",
            crate::ui::escaped(field),
            shown(before),
            shown(after)
        ),
    }
}

/// A staged domain change as JSON reports it, beside the raw row storing it as
/// `managedHostnames` or `routes`: `{"path": "web.domains", "change": "added shop"}`.
#[derive(serde::Serialize)]
pub(crate) struct DomainLine {
    path: String,
    change: String,
}

/// Every domain change among `rows`, as [`changed`] reads them to people.
pub(crate) fn domain_changes<'a>(
    rows: impl IntoIterator<Item = &'a ployz_core::config::ServiceSettingChange>,
) -> Vec<DomainLine> {
    rows.into_iter()
        .filter_map(|row| {
            let domains = ployz_store::DomainChange::of(row)?;
            let service = row.path.split('.').next().unwrap_or_default();
            Some(domains.into_iter().map(move |domain| DomainLine {
                path: format!("{service}.domains"),
                change: domain.to_string(),
            }))
        })
        .flatten()
        .collect()
}

/// A Setting's value as `get` and `inspect` show it, marked when it is the default
/// and followed by the default when it is not: `3 (default 1)`, or `(no default)`.
pub(crate) fn valued(row: &ployz_store::SettingRow) -> String {
    // An Environment's region has no default to compare: unset, its Services run in any region, so a set one
    // reads as itself, never as a region named "default" with no default.
    if row.path.to_string() == "@environment.region" {
        return if row.value.is_null() {
            "any region".to_owned()
        } else {
            plain(&row.value)
        };
    }
    let value = plain(&row.value);
    // A variable or mount (`SERVICE.env.KEY`, `SERVICE.mounts.VOLUME`) has no
    // default: unset removes it.
    let removable = row.path.to_string().split('.').count() > 2;
    if row.value == row.default {
        format!("{value} (default)")
    } else if removable && row.default.is_null() {
        value
    } else if row.default.is_null() {
        format!("{value} (no default)")
    } else {
        format!("{value} (default {})", plain(&row.default))
    }
}

/// A value on one line as JSON writes it, text quoted, so `""` and `"-"` never read as
/// unset, which alone shows as `-`.
pub(crate) fn plain(value: &serde_json::Value) -> String {
    if value.is_null() {
        "-".to_owned()
    } else {
        crate::ui::escaped(&value.to_string())
    }
}

/// A Service's domains on one line, `shop.acme.ployz.app, app.example.com (port 3000)`,
/// or `-` when it has none.
pub(crate) fn domain_list(domains: &[ployz_store::DomainRow], service: &str) -> String {
    let listed: Vec<String> = domains
        .iter()
        .filter(|row| row.domain.service.as_str() == service)
        .map(|row| match row.domain.port {
            Some(port) => format!("{} (port {port})", row.domain.shown()),
            None => row.domain.shown().to_owned(),
        })
        .collect();
    if listed.is_empty() {
        "-".to_owned()
    } else {
        listed.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_staged_domain_change_reads_as_a_domain() {
        let row: ployz_core::config::ServiceSettingChange = serde_json::from_value(serde_json::json!({
            "path": "web.managedHostnames", "kind": "update", "canRestore": false,
            "before": [{ "prefix": "web", "targetPort": null }],
            "after": [{ "prefix": "web", "targetPort": null }, { "prefix": "shop", "targetPort": null }],
        }))
        .unwrap();
        assert_eq!(
            changed(&row, &[]),
            ["web.domains: added shop.<cluster domain>"]
        );
        let named: ployz_core::config::ServiceSettingChange = serde_json::from_value(json!({
            "path": "web.managedHostnames", "kind": "update", "canRestore": false,
            "before": [], "after": [{ "prefix": "shop", "targetPort": 8080 }],
            "hostnames": { "shop": "shop.acme.ployz.app" },
        }))
        .unwrap();
        assert_eq!(
            changed(&named, &[]),
            ["web.domains: added shop.acme.ployz.app on port 8080"]
        );
        let route: ployz_core::config::ServiceSettingChange =
            serde_json::from_value(serde_json::json!({
                "path": "web.routes.r1", "kind": "remove", "canRestore": false,
                "before": { "id": "r1", "hostname": "app.example.com", "targetPort": null },
                "after": null,
            }))
            .unwrap();
        assert_eq!(
            changed(&route, &[]),
            ["web.domains: removed app.example.com"]
        );
    }

    fn row(path: &str, before: serde_json::Value, after: serde_json::Value) -> Vec<String> {
        changed(
            &serde_json::from_value(json!({
                "path": path, "kind": "update", "canRestore": false,
                "before": before, "after": after,
            }))
            .unwrap(),
            &[],
        )
    }

    #[test]
    fn a_staged_image_change_reads_as_the_setting_set_names() {
        let source = |image: &str| json!({ "type": "image", "credentials": false, "image": image });
        assert_eq!(
            row(
                "web.source",
                source("traefik/whoami:v1.10.0"),
                source("traefik/whoami:v1.10.1")
            ),
            [r#"web.image: "traefik/whoami:v1.10.0" → "traefik/whoami:v1.10.1""#]
        );
        assert_eq!(
            row(
                "web.source",
                json!({ "type": "git", "repository": "acme/web", "rootDir": "/" }),
                json!({ "type": "git", "repository": "acme/web", "rootDir": "/api" })
            ),
            [r#"web.rootDir: "/" → "/api""#]
        );
        assert_eq!(
            row("db.source", source("postgres:16"), json!(null)),
            [r#"db.image: "postgres:16" → null"#]
        );
    }

    #[test]
    fn a_staged_config_change_reads_by_name_not_id() {
        let at = "configs.@765c98f1-af88-4dc8-a63d-6f8127034983";
        assert_eq!(
            row(&format!("{at}.name"), json!("app"), json!("app2")),
            [r#"name: "app" → "app2""#]
        );
        let file = json!({ "content": "", "gid": 0, "mode": "0444", "uid": 0 });
        assert_eq!(
            row(&format!("{at}.files.empty.txt"), file.clone(), json!(null)),
            ["files: removed empty.txt"]
        );
        assert_eq!(
            row(&format!("{at}.files.run.sh"), json!(null), file),
            ["files: added run.sh"]
        );
        assert_eq!(
            row(
                &format!("{at}.files.a\u{202E}b.txt"),
                json!(null),
                json!({})
            ),
            [r"files: added a\u{202e}b.txt"]
        );
    }

    #[test]
    fn a_staged_config_mount_reads_by_its_config_name() {
        let mount = |config_name: Option<&str>| {
            changed(
                &serde_json::from_value(json!({
                    "path": "web.configs.@a18f32f9-1b2c-4d3e-8f40-5a6b7c8d9e0f",
                    "configName": config_name, "kind": "add", "canRestore": true,
                    "before": null, "after": "/etc/sentry",
                }))
                .unwrap(),
                &[],
            )
        };
        assert_eq!(
            mount(Some("sentry")),
            [r#"web.configs.sentry: null → "/etc/sentry""#]
        );
        assert_eq!(
            mount(None),
            [r#"web.configs.@a18f32f9-1b2c-4d3e-8f40-5a6b7c8d9e0f: null → "/etc/sentry""#]
        );
    }

    #[test]
    fn a_staged_memory_limit_reads_with_its_unit() {
        let row: ployz_core::config::ServiceSettingChange = serde_json::from_value(json!({
            "path": "web.memLimit", "kind": "update", "canRestore": true,
            "before": null, "after": 0.201_326_592,
        }))
        .unwrap();
        assert_eq!(changed(&row, &[]), ["web.memLimit: null → 192 MiB"]);
    }

    #[test]
    fn a_staged_cpu_limit_reads_in_vcpus() {
        let row = |before: serde_json::Value, after: serde_json::Value| {
            let row: ployz_core::config::ServiceSettingChange = serde_json::from_value(json!({
                "path": "web.cpuLimit", "kind": "update", "canRestore": true,
                "before": before, "after": after,
            }))
            .unwrap();
            changed(&row, &[])
        };
        assert_eq!(
            row(json!(0.5), json!(2)),
            ["web.cpuLimit: 0.5 vCPUs → 2 vCPUs"]
        );
        assert_eq!(row(json!(null), json!(1)), ["web.cpuLimit: null → 1 vCPU"]);
    }

    #[test]
    fn a_set_setting_without_a_default_says_it_has_none() {
        let row = |path: &str, value: serde_json::Value| ployz_store::SettingRow {
            path: ployz_store::SettingPath::parse(path).unwrap(),
            value,
            default: serde_json::Value::Null,
            apply: serde_json::from_value(json!("staged")).unwrap(),
            row: None,
            note: None,
        };
        assert_eq!(
            valued(&row("web.startCommand", json!("npm start"))),
            "\"npm start\" (no default)"
        );
        assert_eq!(valued(&row("web.startCommand", json!(null))), "- (default)");
        assert_eq!(valued(&row("web.env.PORT", json!("3000"))), "\"3000\"");
        assert_eq!(valued(&row("web.mounts.data", json!("/data"))), "\"/data\"");
        assert_eq!(
            valued(&row("@environment.region", json!("default"))),
            "\"default\""
        );
        assert_eq!(
            valued(&row("@environment.region", json!(null))),
            "any region"
        );
    }

    #[test]
    fn empty_text_a_dash_and_unset_read_differently() {
        let shown = [plain(&json!("")), plain(&json!("-")), plain(&json!(null))];
        assert_eq!(shown, ["\"\"", "\"-\"", "-"]);
    }

    #[test]
    fn a_value_escapes_what_would_steer_the_terminal() {
        let value = json!("مرحبا\u{202E}evil\u{200B}\u{85}\t");
        let escaped = r#""مرحبا\u{202e}evil\u{200b}\u{85}\t""#;
        assert_eq!(plain(&value), escaped);
        assert_eq!(shown(&value), escaped);
        let long = shown(&json!(format!("\u{202E}{}", "a".repeat(100))));
        assert!(long.starts_with(r#""\u{202e}aaa"#), "{long}");
    }

    #[test]
    fn a_long_value_is_cut_within_80_columns_never_inside_an_escape() {
        let cut = |before: usize, odd: char| {
            shown(&json!(format!(
                "{}{odd}{}",
                "a".repeat(before),
                "z".repeat(90 - before)
            )))
        };
        let a = |count| "a".repeat(count);
        assert_eq!(cut(78, '\u{202E}'), format!("\"{}… (91 chars)", a(78)));
        assert_eq!(cut(75, '\u{202E}'), format!("\"{}… (91 chars)", a(75)));
        assert_eq!(
            cut(71, '\u{202E}'),
            format!(r#""{}\u{{202e}}… (91 chars)"#, a(71))
        );
        assert_eq!(cut(78, '\n'), format!("\"{}… (91 chars)", a(78)));
        assert_eq!(cut(76, '\u{1}'), format!("\"{}… (91 chars)", a(76)));
        assert_eq!(
            cut(70, '\u{1}'),
            format!(r#""{}\u0001zzz… (91 chars)"#, a(70))
        );
        assert_eq!(cut(78, '界'), format!("\"{}… (91 chars)", a(78)));
        assert_eq!(cut(78, '😀'), format!("\"{}… (91 chars)", a(78)));
        assert_eq!(cut(77, '界'), format!("\"{}界… (91 chars)", a(77)));
    }

    #[test]
    fn a_value_is_measured_in_columns_by_grapheme() {
        let wide = |c: &str| format!("\"{}… (40 chars)", c.repeat(39));
        assert_eq!(shown(&json!("界".repeat(40))), wide("界"));
        assert_eq!(shown(&json!("😀".repeat(40))), wide("😀"));
        let marked = format!("{}\u{301}\u{301}\u{301}", "a".repeat(78));
        assert_eq!(shown(&json!(marked)), format!("\"{marked}\""));
        let heart = format!("{}\u{2764}\u{FE0F}", "a".repeat(78));
        assert_eq!(
            shown(&json!(heart)),
            format!("\"{}… (80 chars)", "a".repeat(78))
        );
    }

    /// The cut to 80 columns would drop the `@` the mask finds user info by.
    #[test]
    fn a_long_value_is_masked_before_it_is_cut() {
        let password = "p".repeat(100);
        let long = shown(&json!(format!(
            "postgres://app:{password}@db.internal/shop"
        )));
        assert!(!long.contains("ppp"), "{long}");
        assert!(
            long.starts_with("\"postgres://<USER>:<PASSWORD>@db"),
            "{long}"
        );
    }

    #[test]
    fn a_long_value_shows_its_start_and_length() {
        assert_eq!(shown(&json!("short")), "\"short\"");
        assert_eq!(shown(&json!(3)), "3");
        let long = shown(&json!("a".repeat(70_000)));
        assert!(long.starts_with("\"aaa"));
        assert!(long.ends_with("… (70000 chars)"));
        assert!(long.chars().count() < 100);
    }
}
