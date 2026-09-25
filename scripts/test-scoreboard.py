#!/usr/bin/env python3
"""Scoreboard for the test-audit campaign: size, speed, and confidence of the test suites.

  snapshot NAME           measure both suites, write .scratch/test-audit/NAME.json
  compare BASE HEAD       print deltas; exit 1 if uncovered lines grew past the gate
  mutants-baseline FILE [NEXTEST_ARGS...]   freeze the caught-mutant set for one core source file
  mutants-check FILE [NEXTEST_ARGS...]      re-run it; exit 1 if any frozen mutant now survives

NEXTEST_ARGS narrow the tests run per mutant (e.g. -E 'binary_id(ployz::deploy_plan)');
pass the same ones to baseline and check. Mutation runs in place, so don't edit the tree meanwhile.

Ignored Rust tests (real-infra rungs) are out of scope; they neither count nor run.
"""
import collections
import json
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORE = ROOT / "core"
DASHBOARD = ROOT / "dashboard"
OUT = ROOT / ".scratch/test-audit"
COVERAGE_GATE = 1.0  # max allowed growth of uncovered lines, in percent
RUST_FEATURES = ["--workspace", "--all-features"]


def run(cmd, cwd, **kwargs):
    started = time.monotonic()
    result = subprocess.run(cmd, cwd=cwd, text=True, capture_output=True, **kwargs)
    if result.returncode != 0:
        sys.exit(f"failed: {' '.join(cmd)}\n{result.stdout[-4000:]}\n{result.stderr[-4000:]}")
    return result.stdout, time.monotonic() - started


def block_lines(lines, start):
    """Lines in the item starting at `start`: one line for `mod x;`, else until braces balance."""
    depth = 0
    for index in range(start, len(lines)):
        depth += lines[index].count("{") - lines[index].count("}")
        if depth <= 0 and ("{" in "".join(lines[start:index + 1]) or lines[index].rstrip().endswith(";")):
            return index - start + 1
    return len(lines) - start


def rust_test_loc():
    # ponytail: brace counting ignores braces inside strings; deltas between snapshots are what matter.
    total = 0
    for path in (CORE / "crates").rglob("*.rs"):
        relative = path.relative_to(CORE / "crates").parts
        text = path.read_text().splitlines()
        if "tests" in relative[:-1] or path.name == "tests.rs" or path.name.endswith("_tests.rs"):
            total += len(text)
            continue
        index = 0
        while index < len(text):
            if text[index].strip() == "#[cfg(test)]":
                item = index + 1
                while item < len(text) and text[item].lstrip().startswith("#["):
                    item += 1
                span = block_lines(text, item) if item < len(text) else 0
                total += span
                index = item + span
            else:
                index += 1
    return total


def dashboard_test_loc():
    src = DASHBOARD / "src"
    files = {*src.rglob("*.test.ts"), *src.rglob("*.test.tsx"), *(src / "test").rglob("*.ts")}
    return sum(len(path.read_text().splitlines()) for path in files)


def rust_snapshot():
    listing, _ = run(["cargo", "nextest", "list", *RUST_FEATURES, "--message-format", "json"], CORE)
    suites = json.loads(listing)["rust-suites"].values()
    count = sum(not case["ignored"] for suite in suites for case in suite["testcases"].values())
    # Warm build so the timed run measures tests, not compilation.
    run(["cargo", "nextest", "run", *RUST_FEATURES, "--no-run"], CORE)
    _, seconds = run(["cargo", "nextest", "run", *RUST_FEATURES, "--no-fail-fast"], CORE)
    report = OUT / "core-coverage.json"
    # Instrumented binaries run slower; retries absorb node-smoke timeouts without changing covered lines.
    run(["cargo", "llvm-cov", "nextest", *RUST_FEATURES, "--retries", "2", "--json", "--summary-only", "--output-path", str(report)], CORE)
    lines = json.loads(report.read_text())["data"][0]["totals"]["lines"]
    return {"tests": count, "test_loc": rust_test_loc(), "seconds": round(seconds, 1),
            "line_coverage": round(lines["percent"], 2), "lines_covered": lines["covered"], "lines_total": lines["count"]}


def dashboard_snapshot():
    listing, _ = run(["pnpm", "-s", "vitest", "list", "--json"], DASHBOARD)
    count = len(json.loads(listing))
    _, seconds = run(["pnpm", "-s", "vitest", "run"], DASHBOARD)
    report = OUT / "dashboard-coverage"
    run(["pnpm", "-s", "vitest", "run", "--coverage.enabled", "--coverage.provider=v8",
         "--coverage.include=src/**", "--coverage.reporter=json-summary",
         f"--coverage.reportsDirectory={report}"], DASHBOARD)
    lines = json.loads((report / "coverage-summary.json").read_text())["total"]["lines"]
    return {"tests": count, "test_loc": dashboard_test_loc(), "seconds": round(seconds, 1),
            "line_coverage": round(lines["pct"], 2), "lines_covered": lines["covered"], "lines_total": lines["total"]}


def snapshot(name):
    OUT.mkdir(parents=True, exist_ok=True)
    head, _ = run(["git", "rev-parse", "--short", "HEAD"], ROOT)
    result = {"commit": head.strip(), "core": rust_snapshot(), "dashboard": dashboard_snapshot()}
    (OUT / f"{name}.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


def uncovered(suite):
    # ponytail: pre-lines_total snapshots derive the total from the rounded percentage (±1 line).
    total = suite.get("lines_total") or round(suite["lines_covered"] * 100 / suite["line_coverage"])
    return total - suite["lines_covered"]


def compare(base_name, head_name):
    base, head = (json.loads((OUT / f"{name}.json").read_text()) for name in (base_name, head_name))
    failed = False
    print(f"{'':10} {'metric':14} {'base':>10} {'head':>10} {'delta':>9}")
    for suite in ("core", "dashboard"):
        for metric in ("tests", "test_loc", "seconds", "line_coverage", "lines_covered"):
            before, after = base[suite][metric], head[suite][metric]
            delta = f"{(after - before) / before * 100:+.1f}%" if metric != "line_coverage" else f"{after - before:+.2f}pt"
            print(f"{suite:10} {metric:14} {before:>10} {after:>10} {delta:>9}")
        # Deleting covered dead code lowers covered lines legitimately; uncovered lines must not grow.
        # Untested files still count as uncovered (llvm-cov and Vitest 4 both report every included file).
        before, after = uncovered(base[suite]), uncovered(head[suite])
        print(f"{suite:10} {'lines_uncovered':14} {before:>10} {after:>10} {after - before:>+9}")
        if after > before * (1 + COVERAGE_GATE / 100):
            print(f"FAIL {suite}: uncovered lines grew more than {COVERAGE_GATE}%")
            failed = True
    sys.exit(1 if failed else 0)


def mutant_key(name):
    # Drop line:col so deleting tests or dead code above a mutant does not rename it.
    return re.sub(r":\d+:\d+:", ":", name)


def mutant_output(target):
    return OUT / "mutants" / str(target).replace("/", "__")


def mutants(path, *nextest_args):
    """Run cargo-mutants on one file; return the multiset of caught mutant keys."""
    target = (Path.cwd() / path).resolve().relative_to(CORE)
    output = mutant_output(target)
    output.mkdir(parents=True, exist_ok=True)
    # In place reuses the warm target dir; copying the tree would cold-build every job.
    result = subprocess.run(["cargo", "mutants", "--file", str(target), "--test-tool", "nextest", "--all-features",
                             "--output", str(output), "--in-place", "--no-shuffle", "--", *nextest_args],
                            cwd=CORE, text=True, capture_output=True)
    # 2 = some mutants missed, 3 = some timed out: both still produce outcomes to compare.
    if result.returncode not in (0, 2, 3):
        sys.exit(f"cargo mutants failed ({result.returncode}):\n{result.stdout[-4000:]}\n{result.stderr[-4000:]}")
    outcomes = json.loads((output / "mutants.out/outcomes.json").read_text())["outcomes"]
    caught = collections.Counter()
    for outcome in outcomes:
        scenario = outcome["scenario"]
        if isinstance(scenario, dict) and outcome["summary"] == "CaughtMutant":
            caught[mutant_key(scenario["Mutant"]["name"])] += 1
    return target, caught


def mutants_baseline(path, *nextest_args):
    target, caught = mutants(path, *nextest_args)
    frozen = mutant_output(target).with_suffix(".frozen.json")
    frozen.write_text(json.dumps(dict(caught), indent=2) + "\n")
    print(f"{target}: froze {sum(caught.values())} caught mutants")


def mutants_check(path, *nextest_args):
    target, caught = mutants(path, *nextest_args)
    frozen = collections.Counter(json.loads(mutant_output(target).with_suffix(".frozen.json").read_text()))
    lost = frozen - caught
    for key in sorted(lost):
        print(f"LOST {key}")
    print(f"{target}: {sum(frozen.values()) - sum(lost.values())}/{sum(frozen.values())} frozen mutants still caught")
    sys.exit(1 if lost else 0)


if __name__ == "__main__":
    commands = {"snapshot": snapshot, "compare": compare,
                "mutants-baseline": mutants_baseline, "mutants-check": mutants_check}
    if len(sys.argv) < 3 or sys.argv[1] not in commands:
        sys.exit(__doc__)
    commands[sys.argv[1]](*sys.argv[2:])
