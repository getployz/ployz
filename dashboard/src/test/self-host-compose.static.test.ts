import { readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, it } from "vitest";

const COMPOSE = readFileSync(join(process.cwd(), "self-host/compose.yml"), "utf8");

// Docker-published ports skip host firewalls like UFW, so a port published on every
// interface would bypass the TLS proxy and its rate limits.
it("publishes self-host ports on loopback by default", () => {
  const published = [...COMPOSE.matchAll(/^\s+ports:\n((?:\s+(?:#|- ).*\n)+)/gm)].flatMap((match) =>
    [...(match[1] ?? "").matchAll(/^\s+- (.+)$/gm)].map((item) => (item[1] ?? "").replace(/^['"]|['"]$/g, "")),
  );
  expect(published.length).toBeGreaterThan(0);
  for (const port of published) {
    expect(port).toMatch(/^(127\.0\.0\.1|\$\{\w+:-127\.0\.0\.1\}):/);
  }
});
