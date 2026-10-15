#!/usr/bin/env bash
# Vendors the isbot pattern list and fixtures into ployzd's bot filter, rewritten for the `regex` crate.
#
# ISBOT_EXTRA_PATTERN appends one more pattern before validation, to prove a rejected pattern fails the run:
#   ISBOT_EXTRA_PATTERN='(?<=x)y' core/scripts/vendor-isbot.sh

set -euo pipefail

ISBOT_SHA=3f9c982acd95b1be033fa3c14b2a7ece565ba10a
SOURCE="https://github.com/omrilotan/isbot/blob/$ISBOT_SHA/src/patterns.json"
RAW="https://raw.githubusercontent.com/omrilotan/isbot/$ISBOT_SHA"

core=$(cd "$(dirname "$0")/.." && pwd)
dest="$core/crates/ployzd/src/observe/bots"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for f in src/patterns.json fixtures/crawlers.yml fixtures/browsers.yml; do
    curl -sfL -o "$work/$(basename "$f")" "$RAW/$f"
done

python3 -I - "$work" "$SOURCE" "${ISBOT_EXTRA_PATTERN:-}" <<'PY'
import json, sys, yaml

work, source, extra = sys.argv[1:4]

# JavaScript spellings the `regex` crate rejects, each replaced by a lookaround-free form that
# matches the same user agents. Every key must exist upstream, so a changed list fails here.
REWRITES = {
    r"(?:^|[^g])news(?!sapphire)":
        r"(?:^|[^g])news(?:$|[^s]|s(?:$|[^a]|a(?:$|[^p]|p(?:$|[^p]|p(?:$|[^h]|h(?:$|[^i]|i(?:$|[^r]|r(?:$|[^e]))))))))",
    r"(?<! channel/|google/)google(?!(?:wv|app|/google| pixel))":
        r"(?:^|[^/]|(?:^|[^le])/"
        r"|(?:^|[^e])l/|(?:^|[^n])el/|(?:^|[^n])nel/|(?:^|[^a])nnel/|(?:^|[^h])annel/|(?:^|[^c])hannel/|(?:^|[^ ])channel/"
        r"|(?:^|[^l])e/|(?:^|[^g])le/|(?:^|[^o])gle/|(?:^|[^o])ogle/|(?:^|[^g])oogle/)"
        r"google(?:$|[^wa/ ]|w(?:$|[^v])|a(?:$|[^p]|p(?:$|[^p]))"
        r"|/(?:$|[^g]|g(?:$|[^o]|o(?:$|[^o]|o(?:$|[^g]|g(?:$|[^l]|l(?:$|[^e]))))))"
        r"| (?:$|[^p]|p(?:$|[^i]|i(?:$|[^x]|x(?:$|[^e]|e(?:$|[^l]))))))",
    r"(?<! cu)bots?(?:\b|_)": r"(?:^|[^u]|(?:^|[^c])u|(?:^|[^ ])cu)bots?(?:\b|_)",
    r"(?<!cam)scan": r"(?:^|[^m]|(?:^|[^a])m|(?:^|[^c])am)scan",
    r"(?<!lib)http": r"(?:^|[^b]|(?:^|[^i])b|(?:^|[^l])ib)http",
    r"java(?!;)": r"java(?:$|[^;])",
    r"^mozilla/\d\.\d\s\((?:compatible;)?(?:\s?[\w\d-.]+\/\d+\.\d+)?\)$":
        r"^mozilla/\d\.\d\s\((?:compatible;)?(?:\s?[\w\d.-]+\/\d+\.\d+)?\)$",
}

patterns = json.load(open(f"{work}/patterns.json"))
missing = [k for k in REWRITES if k not in patterns]
if missing:
    sys.exit(f"rewrite keys not upstream: {missing}")
patterns = [REWRITES.get(p, p) for p in patterns]
if extra:
    patterns.append(extra)
if any("\n" in p for p in patterns):
    sys.exit("a pattern spans lines")
with open(f"{work}/patterns.txt", "w") as out:
    out.write(f"# isbot {source} (Unlicense), rewritten by core/scripts/vendor-isbot.sh\n")
    out.writelines(p + "\n" for p in patterns)

for name in ("crawlers", "browsers"):
    fixture = yaml.safe_load(open(f"{work}/{name}.yml"))
    with open(f"{work}/{name}.txt", "w") as out:
        out.write(f"# isbot fixtures/{name}.yml at {source.split('/blob/')[1].split('/')[0]} (Unlicense)\n")
        for agents in fixture.values():
            out.writelines(ua + "\n" for ua in agents if ua.strip() and "\n" not in ua)
PY

(cd "$core" && PLOYZ_BOTS_PATTERNS="$work/patterns.txt" cargo nextest run -p ployzd --no-tests=fail \
    -E 'test(=observe::bots::tests::patterns_compile)')

mkdir -p "$dest/fixtures"
cp "$work/patterns.txt" "$dest/patterns.txt"
cp "$work/crawlers.txt" "$work/browsers.txt" "$dest/fixtures/"
