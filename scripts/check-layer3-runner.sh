#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
RUNNER=$ROOT/scripts/run-layer3-tests.sh

fail() {
    echo "layer3 runner check failed: $*" >&2
    exit 1
}

[ -f "$RUNNER" ] || fail "missing $RUNNER"

listed=$(grep -E '^[[:space:]]*--test ' "$RUNNER" | awk '{print $2}' | sort -u)

required_file=$(mktemp)
trap 'rm -f "$required_file"' EXIT

while IFS= read -r file; do
    rel=${file#"$ROOT"/}
    case "$rel" in
        */src/*)
            python3 - "$file" "$RUNNER" "${rel%%/*}" <<'PYLIB' || fail "$rel has an unregistered informing library test"
import pathlib
import re
import shlex
import sys

source, runner, package = sys.argv[1:]
commands = [shlex.split(line) for line in pathlib.Path(runner).read_text().replace("\\\n", " ").splitlines()]
text = pathlib.Path(source).read_text()
names = re.findall(r'#\[ignore = "informing[^"\n]*"\]\s*(?:async\s+)?fn\s+(\w+)', text)
if len(names) != text.count('ignore = "informing'):
    sys.exit("cannot identify every informing library test")
for name in names:
    if not any(
        command[:3] == ["retry_once", "cargo", "test"]
        and "--package" in command and command[command.index("--package") + 1:][:1] == [package]
        and "--lib" in command and "--exact" in command and "--ignored" in command and "--" in command
        and any(argument.endswith("::" + name) for argument in command[:command.index("--")])
        for command in commands
    ):
        sys.exit(f"missing explicit {package} --lib registration for {name}")
PYLIB
            ;;
        */tests/*/*.rs)
            basename "$(dirname "$rel")"
            ;;
        */tests/*.rs)
            basename "$rel" .rs
            ;;
        *)
            fail "cannot map $rel to a cargo test binary"
            ;;
    esac
done < <(grep -rl --include='*.rs' 'ignore = "informing' "$ROOT/ployz" "$ROOT/ployz-testkit" "$ROOT/ployzd") |
    sort -u > "$required_file"

required=$(cat "$required_file")
missing=$(comm -23 <(printf '%s\n' "$required") <(printf '%s\n' "$listed"))
[ -z "$missing" ] || fail "informing binaries missing from run-layer3-tests.sh: $missing"

extra=$(comm -13 <(printf '%s\n' "$required") <(printf '%s\n' "$listed"))
[ -z "$extra" ] || fail "run-layer3-tests.sh lists binaries with no informing ignore: $extra"
