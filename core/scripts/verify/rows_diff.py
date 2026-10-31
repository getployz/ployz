"""Print each row two `sqlite3 -json` dumps from rows-agree.sh disagree on, by field."""

import json
import sys

KEYS = {
    "cluster": ("key",),
    "machines": ("id",),
    "containers": ("id",),
    "certificates": ("hostname",),
    "volumes": ("machine_id", "name"),
    "ingress_routes": ("machine_id", "service_id"),
}


def rows(path):
    text, decoder, at, found = open(path).read(), json.JSONDecoder(), 0, {}
    while at < len(text):
        if text[at].isspace():
            at += 1
            continue
        batch, at = decoder.raw_decode(text, at)
        for row in batch:
            row = {name.lower(): value for name, value in row.items()}
            table = row["tbl"]
            key = tuple(row[name] for name in KEYS[table])
            found[(table, key)] = row
    return found


def leaves(value, path=""):
    if isinstance(value, str) and value[:1] in "{[":
        try:
            value = json.loads(value)
        except ValueError:
            pass
    if isinstance(value, dict):
        for name, inner in value.items():
            yield from leaves(inner, f"{path}.{name}")
    elif isinstance(value, list):
        for index, inner in enumerate(value):
            yield from leaves(inner, f"{path}[{index}]")
    else:
        yield path, value


def main(reference, other):
    ours, theirs = rows(reference), rows(other)
    for key in sorted(set(ours) | set(theirs), key=str):
        mine, peer = ours.get(key), theirs.get(key)
        if mine == peer:
            continue
        label = f"  {key[0]} {'/'.join(map(str, key[1]))}"
        if mine is None or peer is None:
            print(f"{label}: only on {'the reference' if peer is None else 'this Machine'}")
            continue
        a, b = dict(leaves(mine)), dict(leaves(peer))
        for path in sorted(set(a) | set(b)):
            if a.get(path) != b.get(path):
                print(f"{label} {path}: reference {a.get(path)!r}, here {b.get(path)!r}")


main(*sys.argv[1:])
