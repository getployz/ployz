#!/usr/bin/env bash
# Moves every pin of the getployz/build commit to <commit>: BUILDER_SHA, Cloud's workflow
# templates and the minter's digests of them, this repository's own build workflow, the tests
# that pin it and the docs that print it. Then checks the new commit installs a signed
# getployz/ployz release, and fails, with the edits left for `git checkout`, when it does not.
# The minter needs a deploy for its new digests to apply.

set -euo pipefail

new=${1:-}
[[ $new =~ ^[0-9a-f]{40}$ ]] || {
    echo "usage: $0 <full getployz/build commit>" >&2
    exit 1
}

repo=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$repo"
old=$(sed -n 's/^pub const BUILDER_SHA: &str = "\([0-9a-f]\{40\}\)";$/\1/p' core/crates/ployz-core/src/authority/rule.rs)
[ -n "$old" ] || {
    echo "no BUILDER_SHA in core/crates/ployz-core/src/authority/rule.rs" >&2
    exit 1
}

rule=core/crates/ployz-core/src/authority/rule.rs
sed -i "s/^pub const BUILDER_SHA: &str = \"$old\";\$/pub const BUILDER_SHA: \&str = \"$new\";/" "$rule"
git grep -lz "$old" -- . ':!docs/agents/plans' ":!$rule" ':!core/crates/ployz-core/src/authority/rule_tests.rs' |
    xargs -0 -r sed -i "s/$old/$new/g"
grep -qF "\"$old\"," "$rule" || {
    echo "add $old to RETIRED_BUILDERS in $rule if its workflow installs no Ployz release" >&2
}
node --input-type=module <<'JS'
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
const { GITHUB_WORKFLOW_TEMPLATES } = await import(`${process.cwd()}/dashboard/src/modules/github/github-build-workflow.ts`);
const path = "minter/src/canonical-workflows.json";
const canonical = JSON.parse(readFileSync(path, "utf8"));
canonical.digests = [GITHUB_WORKFLOW_TEMPLATES.build, GITHUB_WORKFLOW_TEMPLATES.rule]
  .map((workflow) => createHash("sha256").update(workflow.replaceAll("\r\n", "\n"), "utf8").digest("hex"));
writeFileSync(path, `${JSON.stringify(canonical, null, 2)}\n`);
JS
git grep -n "$new" -- . ':!docs/agents/plans'
bash core/scripts/check-builder-pin.sh
