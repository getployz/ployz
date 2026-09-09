//! Railpack recipe selection, exclusions, and private variable capture.

use super::*;

#[test]
fn railpack_selection_keeps_explicit_dockerfiles_and_refuses_check() {
    let root = std::env::temp_dir().join(format!("ployz-recipe-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    for (declaration, has_file, railpack) in [
        ("build: .", false, true),
        ("build: .", true, false),
        ("build: {context: ., dockerfile: Dockerfile}", false, false),
        ("build: {context: ., x-recipe: railpack}", true, true),
        ("build: {context: ., x-recipe: dockerfile}", false, false),
        (
            "build: {context: ., dockerfile_inline: 'FROM scratch'}",
            false,
            false,
        ),
    ] {
        let _ = fs::remove_file(root.join("Dockerfile"));
        if has_file {
            fs::write(root.join("Dockerfile"), "FROM scratch").unwrap();
        }
        fs::write(
            root.join("compose.yaml"),
            format!("services:\n  app:\n    {declaration}\n"),
        )
        .unwrap();
        let load = LoadOptions {
            working_dir: Some(root.clone()),
            ..Default::default()
        };
        let mut project = load_project(&load).unwrap();
        let options = BuildOptions {
            output: Output::Validate,
            ..Default::default()
        };
        let plan = plan_build(&project, &options).unwrap();
        let error = capture_build(&plan, &options, &mut project)
            .err()
            .map(|error| error.to_string());
        assert_eq!(
            error
                .as_deref()
                .is_some_and(|error| error.contains("Railpack") && error.contains("--check")),
            railpack,
            "{declaration}: {error:?}"
        );
        if !railpack && !has_file && !declaration.contains("inline") {
            assert!(error.is_some());
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[expect(clippy::indexing_slicing, reason = "Fixed capture fixture")]
fn railpack_capture_filters_before_transfer_and_keeps_effective_variables_private() {
    let root = std::env::temp_dir().join(format!("ployz-railpack-capture-{}", std::process::id()));
    fs::create_dir_all(root.join("src/config")).unwrap();
    fs::write(
        root.join("src/.dockerignore"),
        "*.txt\nconfig\n.dockerignore\n",
    )
    .unwrap();
    fs::write(
        root.join("src/config/custom.json"),
        "{ // Railpack permits comments\n\"exclude\": [\"!keep.txt\", \"drop.log\",],\n}",
    )
    .unwrap();
    for name in ["drop.txt", "drop.log"] {
        fs::write(root.join("src").join(name), "excluded-private-bytes").unwrap();
    }
    fs::write(root.join("src/keep.txt"), "captured").unwrap();
    let mut project = parse_normalized(r#"
services:
  api:
    image: example.test/api:check
    build: {context: ./src, args: {MODE: compose}}
    environment: {TOKEN: 'secret://token', MODE: runtime, DEFAULT: service, RAILPACK_CONFIG_FILE: config/custom.json}
secrets:
  token: {x-command: "sh -c 'echo once >> provider-calls; printf private-token'"}
"#, &root).unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    fs::write(root.join("digest"), FIRST_CONTENT).unwrap();
    fs::write(root.join("image"), "example.test/api:check").unwrap();
    let mut captures = Vec::new();
    for value in ["first'$\nline", "changed"] {
        let options = BuildOptions {
            build_args: vec![format!("MODE={value}")],
            ..Default::default()
        };
        let plan = plan_build(&project, &options).unwrap();
        captures.push((value, capture_build(&plan, &options, &mut project).unwrap()));
    }
    project.resolve_secrets().unwrap();
    assert_eq!(
        project.services["api"].container.environment["MODE"],
        "runtime"
    );
    assert_eq!(
        project.services["api"].container.environment["TOKEN"],
        "private-token"
    );
    assert_eq!(
        fs::read_to_string(root.join("provider-calls")).unwrap(),
        "once\n"
    );
    fs::remove_dir_all(root.join("src")).unwrap();
    let mut hashes = Vec::new();
    for (value, capture) in captures {
        fs::write(root.join("preparation-fails"), "").unwrap();
        let failed = capture.execute(Some(&docker)).unwrap_err().to_string();
        assert!(failed.contains("Railpack preparation"), "{failed}");
        fs::remove_file(root.join("preparation-fails")).unwrap();
        let result = one_built(capture.execute(Some(&docker)).unwrap());
        assert_eq!(one_built(capture.execute(Some(&docker)).unwrap()), result);
        assert!(!format!("{result:?}").contains("private-token"));
        let received = root.join("received");
        assert_eq!(
            fs::read_to_string(received.join("keep.txt")).unwrap(),
            "captured"
        );
        assert!(received.join("config/custom.json").exists());
        assert!(received.join(".dockerignore").exists());
        for file in ["drop.txt", "drop.log"] {
            assert!(!received.join(file).exists());
        }
        let bake: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("override.yaml")).unwrap()).unwrap();
        let args = &bake["target"]["api"]["args"];
        assert_eq!(args.as_object().unwrap().len(), 2);
        assert!(!args.to_string().contains(value));
        hashes.push(args["secrets-hash"].as_str().unwrap().to_owned());
        let staged = root.join("relocated/private/railpack/0");
        // BTreeMap order is not the claim: verify the collection of supplied values.
        let mut values = fs::read_dir(&staged)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("secret-"))
            .map(|entry| {
                assert_eq!(
                    entry.metadata().unwrap().permissions().mode() & 0o777,
                    0o600
                );
                fs::read_to_string(entry.path()).unwrap()
            })
            .collect::<Vec<_>>();
        values.sort();
        let mut expected = vec!["config/custom.json", "private-token", "service", value];
        expected.sort();
        assert_eq!(values, expected);
        let source_compose = fs::read_to_string(root.join("relocated/compose.yaml")).unwrap();
        assert!(!source_compose.contains("private-token"));
        assert!(
            !fs::read_to_string(root.join("calls"))
                .unwrap()
                .contains("private-token")
        );
        let captured_root = fs::read_to_string(root.join("capture-root")).unwrap();
        assert!(
            !Path::new(captured_root.trim())
                .join("private/railpack")
                .exists()
        );
        drop(capture);
        assert!(!Path::new(captured_root.trim()).exists());
    }
    assert_ne!(
        hashes[0], hashes[1],
        "changed variables must invalidate frontend cache"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn railpack_refuses_frontend_options_it_cannot_honor() {
    let root = std::env::temp_dir().join(format!("ployz-railpack-options-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    for field in [
        "target: build",
        "labels: {hello: world}",
        "ssh: [default]",
        "network: none",
        "additional_contexts: {other: .}",
        "x-bake: {no-cache-filter: [build]}",
    ] {
        let mut project = parse_normalized(
            &format!("services:\n  api:\n    build: {{context: ., x-recipe: railpack, {field}}}\n"),
            &root,
        )
        .unwrap();
        let options = BuildOptions::default();
        let plan = plan_build(&project, &options).unwrap();
        let error = capture_build(&plan, &options, &mut project)
            .err()
            .expect("ignored option must be refused")
            .to_string();
        assert!(
            error.contains("Railpack") && error.contains(field.split(':').next().unwrap()),
            "{error}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}
