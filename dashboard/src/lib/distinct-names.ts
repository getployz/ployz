/**
 * Names each row after its first part, adding a later part only when it tells the row apart from the rows still
 * sharing its name. Rows whose finished names still match get their position among them, "N of M", and that repeats
 * until every name is unique. Two rows numbered apart never match again, so it ends within one pass per pair of rows.
 */
export function distinctNames(rows: readonly (readonly (string | null)[])[]): string[] {
  let names = rows.map((parts) => {
    let peers = rows.filter((row) => row[0] === parts[0]);
    const name = [parts[0]];
    for (let part = 1; part < parts.length && peers.length > 1; part++) {
      if (peers.every((peer) => peer[part] === parts[part])) continue;
      peers = peers.filter((peer) => peer[part] === parts[part]);
      name.push(parts[part]);
    }
    return name.filter(Boolean).join(", ");
  });
  for (;;) {
    const counts = new Map<string, number>();
    for (const name of names) counts.set(name, (counts.get(name) ?? 0) + 1);
    if (counts.size === names.length) return names;
    const numbered = new Map<string, number>();
    names = names.map((name) => {
      const count = counts.get(name) ?? 1;
      if (count === 1) return name;
      const position = (numbered.get(name) ?? 0) + 1;
      numbered.set(name, position);
      return [name, `${position} of ${count}`].filter(Boolean).join(", ");
    });
  }
}
