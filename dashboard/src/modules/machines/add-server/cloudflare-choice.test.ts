import { expect, test } from "vitest";
import { abandonCloudflare, installGate } from "./cloudflare-choice";

test("Get install command waits for Cloudflare, and a setup left half way falls back to public ports and says so", () => {
  expect(installGate({ at: "off", abandoned: false })).toEqual({ allowed: true, note: null, foundsClosed: false });
  expect(installGate({ at: "token" }).allowed).toBe(false);
  expect(installGate({ at: "zone", zones: ["zenploy.dev"] }).allowed).toBe(false);
  expect(installGate({ at: "ready", zone: "zenploy.dev" })).toEqual({ allowed: true, note: null, foundsClosed: true });
  const abandoned = abandonCloudflare({ at: "zone", zones: ["zenploy.dev"] });
  expect(abandoned).toEqual({ at: "off", abandoned: true });
  expect(installGate(abandoned)).toMatchObject({ allowed: true, foundsClosed: false, note: "Without Cloudflare Tunnel, your servers open ports 80 and 443." });
  expect(abandonCloudflare({ at: "off", abandoned: false })).toEqual({ at: "off", abandoned: false });
});
