import { expect, it } from "vitest";
import { generateTokenName } from "#/modules/machines/waiting/names";

it("names a token with two lowercase words joined by a hyphen", () => {
  const names = Array.from({ length: 500 }, generateTokenName);
  for (const name of names) expect(name).toMatch(/^[a-z]+-[a-z]+$/);
  expect(new Set(names).size).toBeGreaterThan(100);
});
