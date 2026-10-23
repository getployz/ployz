import { expect, it } from "vitest";
import { generateTokenName, machineNameFromHostname } from "#/modules/machines/waiting/names";

it("names a token with two lowercase words joined by a hyphen", () => {
  const names = Array.from({ length: 500 }, generateTokenName);
  for (const name of names) expect(name).toMatch(/^[a-z]+-[a-z]+$/);
  expect(new Set(names).size).toBeGreaterThan(100);
});

// The same table as MachineName::from_hostname in ployz-core, so Cloud plans the name the Machine takes.
it.each([
  ["MY-SERVER", "my-server"],
  ["vultr1", "vultr1"],
  ["my_box__01", "my-box-01"],
  ["web.example.com", "web-example-com"],
  ["--Edge Node--", "edge-node"],
  ["a---b", "a-b"],
  [`${"a".repeat(66)}-tail`, "a".repeat(63)],
  [`${"a".repeat(62)}_b`, "a".repeat(62)],
])("names a Machine from hostname %j", (hostname, name) => {
  expect(machineNameFromHostname(hostname)).toBe(name);
});

it.each(["", "___", "-.-", "ÜÖÄ"])("names no Machine from hostname %j", (hostname) => {
  expect(machineNameFromHostname(hostname)).toBeNull();
});
