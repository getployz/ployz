use std::time::{SystemTime, UNIX_EPOCH};

use clap::ArgMatches;
use ployz_core::MachineSuccess;

use crate::cluster::MachineImagesObservation;

use super::{Error, connect_client, leaf_matches, runtime, string_values};

pub(super) fn list(matches: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(matches);
    let targets = string_values(leaf, "machine");
    let reference = leaf.get_one::<String>("image").cloned();
    let json = leaf.get_one::<String>("output").map(String::as_str) == Some("json");
    let result = runtime()?.block_on(async {
        let mut client = connect_client(
            matches,
            leaf.get_one::<String>("context").map(String::as_str),
        )
        .await?;
        Ok::<_, Error>(crate::image::list(&mut client, reference, &targets).await?)
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let now_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        print!(
            "{}",
            format_images_table(&result.successes, now_unix_seconds)
        );
    }
    for failure in result.failures {
        eprintln!("WARNING: {}: {}", failure.machine_id, failure.error.message);
    }
    for omission in result.omissions {
        eprintln!("WARNING: {omission}: no terminal image-list response");
    }
    Ok(())
}

#[must_use]
fn format_images_table(
    successes: &[MachineSuccess<MachineImagesObservation>],
    now_unix_seconds: u64,
) -> String {
    let mut table = String::from(
        "MACHINE\tCONTAINERD\tID\tREPOSITORY:TAG\tCREATED\tSIZE\tCONTAINERS\tPLATFORMS\n",
    );
    for success in successes {
        for image in &success.value.images.images {
            for tag in image
                .repo_tags
                .iter()
                .map(String::as_str)
                .chain(image.repo_tags.is_empty().then_some("<none>"))
            {
                table.push_str(&format!(
                    "{}\t{}\t{}\t{tag}\t{}\t{}\t{}\t{}\n",
                    success.value.machine_name,
                    success.value.images.containerd_store,
                    image.id,
                    format_created(image.created, now_unix_seconds),
                    format_size(image.size),
                    image.containers,
                    image.platforms.join(","),
                ));
            }
        }
    }
    table
}

#[must_use]
fn format_created(created_unix_seconds: i64, now_unix_seconds: u64) -> String {
    let Ok(created) = u64::try_from(created_unix_seconds) else {
        return "-".into();
    };
    format_relative_ago(now_unix_seconds.saturating_sub(created))
}

#[must_use]
fn format_relative_ago(elapsed_seconds: u64) -> String {
    for (seconds_per_unit, name) in [
        (365 * 24 * 60 * 60, "year"),
        (24 * 60 * 60, "day"),
        (60 * 60, "hour"),
        (60, "minute"),
        (1, "second"),
    ] {
        let count = elapsed_seconds / seconds_per_unit;
        if count == 0 {
            continue;
        }
        return format!("{count} {name}{} ago", if count == 1 { "" } else { "s" });
    }
    "0 seconds ago".into()
}

#[must_use]
fn format_size(bytes: i64) -> String {
    let Ok(bytes) = u64::try_from(bytes) else {
        return "-".into();
    };
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in ["KB", "MB", "GB", "TB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    let rendered = format!("{value:.1}");
    format!("{}{unit}", rendered.strip_suffix(".0").unwrap_or(&rendered))
}

pub(super) fn push(matches: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(matches);
    let image = leaf
        .get_one::<String>("image")
        .ok_or_else(|| Error::usage("image is required"))?;
    let result = runtime()?.block_on(async {
        let mut client = connect_client(
            matches,
            leaf.get_one::<String>("context").map(String::as_str),
        )
        .await?;
        Ok::<_, Error>(
            crate::image::push(
                &mut client,
                image,
                leaf.get_one::<String>("platform").map(String::as_str),
                &string_values(leaf, "machine"),
            )
            .await?,
        )
    })?;
    for success in &result.successes {
        println!("Pushed {image} to {}", success.machine_id);
    }
    for failure in &result.failures {
        eprintln!("WARNING: {}: {}", failure.machine_id, failure.error);
    }
    if result.all_targets_succeeded() {
        Ok(())
    } else {
        Err(Error::usage(format!(
            "image push failed on {} target(s)",
            result.failures.len() + result.omissions.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{
        ImageSummary, MachineId, MachineImages, MachineName, MachineSuccess, PartialResult,
        RpcError,
    };

    use crate::cluster::MachineImagesObservation;

    #[test]
    fn created_column_uses_relative_time_not_the_unix_seconds() {
        assert_eq!(
            format_created(1_700_000_000 - 72, 1_700_000_000),
            "1 minute ago"
        );
        assert_eq!(
            format_created(1_700_000_000 - 2 * 24 * 60 * 60, 1_700_000_000),
            "2 days ago"
        );
        assert_eq!(
            format_created(1_700_000_000, 1_700_000_000),
            "0 seconds ago"
        );
        assert_eq!(format_created(-1, 1_700_000_000), "-");
    }

    #[test]
    fn size_column_uses_1024_based_human_units() {
        assert_eq!(format_size(19_191_186), "18.3MB");
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(1024), "1KB");
        assert_eq!(format_size(10 * 1024), "10KB");
        assert_eq!(format_size(10 * 1024 * 1024), "10MB");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(-1), "-");
    }

    #[test]
    fn images_table_renders_created_and_size_for_humans() {
        let table =
            format_images_table(&[sample_image_success()], 1_785_339_784 + 2 * 24 * 60 * 60);
        assert_eq!(
            table,
            "MACHINE\tCONTAINERD\tID\tREPOSITORY:TAG\tCREATED\tSIZE\tCONTAINERS\tPLATFORMS\n\
             ord1\ttrue\tsha256:c4717a8\ttraefik/whoami:latest\t2 days ago\t18.3MB\t1\tlinux/amd64\n"
        );
        assert!(!table.contains("1785339784"));
        assert!(!table.contains("19191186"));
    }

    #[test]
    fn untagged_image_prints_none_once() {
        let mut success = sample_image_success();
        success
            .value
            .images
            .images
            .first_mut()
            .expect("sample has one image")
            .repo_tags
            .clear();
        let table = format_images_table(&[success], 1_785_339_784);
        assert!(table.contains("\t<none>\t"));
        assert_eq!(table.lines().count(), 2);
    }

    #[test]
    fn images_json_keeps_raw_created_and_size() {
        let result = PartialResult::<_, RpcError> {
            successes: vec![sample_image_success()],
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(
            json.pointer("/successes/0/value/images/images/0/created"),
            Some(&serde_json::json!(1_785_339_784))
        );
        assert_eq!(
            json.pointer("/successes/0/value/images/images/0/size"),
            Some(&serde_json::json!(19_191_186))
        );
    }

    fn sample_image_success() -> MachineSuccess<MachineImagesObservation> {
        MachineSuccess {
            machine_id: MachineId::parse("0".repeat(32)).unwrap(),
            value: MachineImagesObservation {
                machine_name: MachineName::parse("ord1").unwrap(),
                images: MachineImages {
                    containerd_store: true,
                    images: vec![ImageSummary {
                        id: "sha256:c4717a8".into(),
                        repo_tags: vec!["traefik/whoami:latest".into()],
                        created: 1_785_339_784,
                        size: 19_191_186,
                        containers: 1,
                        platforms: vec!["linux/amd64".into()],
                    }],
                },
            },
        }
    }
}
