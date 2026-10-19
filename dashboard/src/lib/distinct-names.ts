/**
 * Names each row after its first part, adding a later part only when it tells the row apart from the rows still
 * sharing its name, and its position among them when no part does. Every name in the list comes out unique.
 */
export function distinctNames(rows: readonly (readonly (string | null)[])[]): string[] {
  return rows.map((parts) => {
    let peers = rows.filter((row) => row[0] === parts[0]);
    const name = [parts[0]];
    for (let part = 1; part < parts.length && peers.length > 1; part++) {
      if (peers.every((peer) => peer[part] === parts[part])) continue;
      peers = peers.filter((peer) => peer[part] === parts[part]);
      name.push(parts[part]);
    }
    if (peers.length > 1) name.push(`${peers.indexOf(parts) + 1} of ${peers.length}`);
    return name.filter(Boolean).join(", ");
  });
}
