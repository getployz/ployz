#!/usr/bin/env python3
"""Keep Cloud's SDK and native bindings on the engine version."""
import json
from pathlib import Path
import sys
import tomllib


def check(manifest, package):
    version = tomllib.loads(manifest.read_text())["workspace"]["package"]["version"]
    cloud = json.loads(package.read_text())
    pins = {"@ployz/sdk": cloud["dependencies"].get("@ployz/sdk")}
    if pins["@ployz/sdk"] == "link:../core/crates/ployz-sdk":
        sdk = manifest.parent / "crates/ployz-sdk/package.json"
        pins["@ployz/sdk"] = json.loads(sdk.read_text())["version"]
    for platform in ("darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64"):
        name = f"@ployz/sdk-{platform}"
        pins[name] = cloud.get("optionalDependencies", {}).get(name)
    for name, pin in pins.items():
        if pin != version:
            raise ValueError(f"{name}: Cloud pins {pin!r}; expected engine version {version}")


if __name__ == "__main__":
    root = Path(__file__).resolve().parent.parent
    try:
        check(root / "core/Cargo.toml", root / "dashboard/package.json")
    except (ValueError, KeyError, OSError) as error:
        sys.exit(str(error))
