import { expect, it } from "vitest";
import { distinctNames } from "./distinct-names";

it("keeps a unique row's name to its first part, and adds only the parts that tell rows apart", () => {
  expect(distinctNames([
    ["ada@example.com", "from Ada", "Member"],
    ["link expiring Oct 21, 2026", "from Ada", "Member"],
    ["link expiring Oct 21, 2026", "from Grace", "Member"],
    ["link expiring Oct 21, 2026", "from Grace", "Admin"],
  ])).toEqual([
    "ada@example.com",
    "link expiring Oct 21, 2026, from Ada",
    "link expiring Oct 21, 2026, from Grace, Member",
    "link expiring Oct 21, 2026, from Grace, Admin",
  ]);
});

it("numbers rows whose parts differ but whose names come out the same", () => {
  expect(distinctNames([
    ["link expiring Oct 21, 2026", "from Ada, Member", "Member"],
    ["link expiring Oct 21, 2026", "from Ada", "Member"],
    ["link expiring Oct 21, 2026", "from Ada", "Admin"],
  ])).toEqual([
    "link expiring Oct 21, 2026, from Ada, Member, 1 of 2",
    "link expiring Oct 21, 2026, from Ada, Member, 2 of 2",
    "link expiring Oct 21, 2026, from Ada, Admin",
  ]);
});

it("numbers again when a position matches a name that already had it", () => {
  const names = distinctNames([["A", null], ["A", null], ["A", "1 of 2"]]);
  expect(new Set(names).size).toBe(3);
  expect(names).toEqual(["A, 1 of 2, 1 of 2", "A, 2 of 2", "A, 1 of 2, 2 of 2"]);
});

it("gives every row a different name, whatever the parts", () => {
  const pieces = [null, "", "A", "B", "1 of 2", "2 of 2", "A, 1 of 2", "A, B", "1 of 3", "A, 1 of 2, 1 of 2"];
  let seed = 7;
  const pick = () => {
    seed = (seed * 1103515245 + 12345) % 2147483648;
    return pieces[seed % pieces.length] ?? null;
  };
  for (let run = 0; run < 3000; run++) {
    const rows = Array.from({ length: 1 + (run % 8) }, () => Array.from({ length: 1 + (run % 3) }, pick));
    const names = distinctNames(rows);
    expect(new Set(names).size, JSON.stringify(rows)).toBe(rows.length);
  }
});
