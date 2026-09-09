#!/usr/bin/env python3
"""Rung 1: only protected main pushes receive shared-cache write credentials."""
from itertools import product
from pathlib import Path
import re
from types import SimpleNamespace


root = Path(__file__).resolve().parents[1]
for name in ("ci", "cloud", "release-contracts", "publish-sdk"):
    workflow = (root / f".github/workflows/{name}.yml").read_text()
    expressions = dict(re.findall(r"^  (KACHE_S3_\w+): \$\{\{ (.*?) \}\}$", workflow, re.M))
    for event, ref, protected in product(
        ("pull_request", "pull_request_target", "push", "release", "schedule", "workflow_dispatch"),
        ("refs/heads/main", "refs/heads/feature", "refs/tags/v1.2.3"),
        (False, True),
    ):
        secrets = SimpleNamespace(
            KACHE_S3_ACCESS_KEY_ID="write-key",
            KACHE_S3_SECRET_ACCESS_KEY="write-secret",
            KACHE_S3_READ_ACCESS_KEY_ID="read-key",
            KACHE_S3_READ_SECRET_ACCESS_KEY="read-secret",
        )
        context = {
            "github": SimpleNamespace(event_name=event, ref=ref, ref_protected=protected),
            "secrets": secrets,
            "vars": SimpleNamespace(KACHE_S3_BUCKET="test-bucket"),
        }
        writing = event == "push" and ref == "refs/heads/main" and protected
        prefix = "write" if writing else "read"
        expected = {
            "KACHE_S3_ACCESS_KEY_ID": f"{prefix}-key",
            "KACHE_S3_SECRET_ACCESS_KEY": f"{prefix}-secret",
            "KACHE_S3_BUCKET": "test-bucket",
        }
        for key, wanted in expected.items():
            expression = expressions[key].replace("&&", " and ").replace("||", " or ")
            actual = eval(expression, {"__builtins__": {}}, context)
            assert actual == wanted, (name, event, ref, protected, key, actual, wanted)

release = (root / ".github/workflows/release.yml").read_text()
for key in ("KACHE_S3_READ_ACCESS_KEY_ID", "KACHE_S3_READ_SECRET_ACCESS_KEY"):
    assert f"{key}: ${{{{ secrets.{key} }}}}" in release
print("PASS: protected main pushes write; PRs/releases read")
