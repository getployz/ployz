use clap::ArgMatches;

use super::{Error, connect_client, leaf_matches, runtime, string_values};

pub(super) fn list(matches: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(matches);
    let targets = string_values(leaf, "machine");
    let reference = leaf.get_one::<String>("image").cloned();
    let result = runtime()?.block_on(async {
        let client = connect_client(
            matches,
            leaf.get_one::<String>("context").map(String::as_str),
        )
        .await?;
        client
            .list_images(reference, &targets)
            .await
            .map_err(Error::from)
    })?;
    println!("MACHINE\tCONTAINERD\tID\tREPOSITORY:TAG\tCREATED\tSIZE\tCONTAINERS\tPLATFORMS");
    for success in result.successes {
        for image in success.value.images.images {
            let tags = if image.repo_tags.is_empty() {
                vec!["<none>".into()]
            } else {
                image.repo_tags
            };
            for tag in tags {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    success.value.machine_name,
                    success.value.images.containerd_store,
                    image.id,
                    tag,
                    image.created,
                    image.size,
                    image.containers,
                    image.platforms.join(",")
                );
            }
        }
    }
    for failure in result.failures {
        eprintln!("WARNING: {}: {}", failure.machine_id, failure.error.message);
    }
    for omission in result.omissions {
        eprintln!("WARNING: {omission}: no terminal image-list response");
    }
    Ok(())
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
        .await
        .map_err(|error| crate::image::PushError::Cluster(error.to_string()))?;
        crate::image::push(
            &mut client,
            image,
            leaf.get_one::<String>("platform").map(String::as_str),
            &string_values(leaf, "machine"),
        )
        .await
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
