import { expect, it } from "vitest";
import { type UpgradeFailure, upgradeFailedMail } from "#/modules/mail/templates/servers/upgrade-failed";

const failure = (overrides: Partial<UpgradeFailure>): UpgradeFailure => ({
  machineId: "a".repeat(32), name: "web-1", fromVersion: "0.2.5", targetVersion: "0.2.6", outcome: "failed", stage: null, error: null, ...overrides,
});
const mail = (...failures: [UpgradeFailure, ...UpgradeFailure[]]) =>
  upgradeFailedMail({ organization: { name: "Acme", slug: "acme" }, failures, origin: "https://ployz.dev" });
const left = (overrides: Partial<UpgradeFailure>) => mail(failure(overrides)).facts?.[0]?.value;

it("names the server and the release, and says the release is skipped only when the attempt knew it", () => {
  expect(mail(failure({}))).toMatchObject({
    subject: "web-1 didn't upgrade to 0.2.6",
    title: "web-1 didn't upgrade",
    paragraphs: ["Ployz stopped the automatic upgrade in Acme. Automatic upgrades skip 0.2.6 until a newer release is out or you upgrade a server to it yourself."],
    action: { label: "View server", url: `https://ployz.dev/cloud/acme/~/servers/${"a".repeat(32)}` },
  });
  expect(mail(failure({ targetVersion: null }))).toMatchObject({
    subject: "web-1 didn't upgrade",
    paragraphs: ["Ployz stopped the automatic upgrade in Acme."],
  });
});

it("says where each server was left instead of the stage it stopped at", () => {
  expect(left({ stage: "readiness", error: "readiness timed out; restored 0.2.5" })).toBe("Rolled back to 0.2.5");
  expect(left({ stage: "restarting", error: "unit failed; restore failed: no previous release" })).toBe("Couldn't roll back");
  expect(left({ stage: "acquiring", error: "download failed" })).toBe("Still on 0.2.5");
  expect(left({ stage: "acquiring", error: "download failed", fromVersion: "" })).toBe("Nothing changed");
  expect(left({ outcome: "interrupted", stage: "verifying" })).toBe("Still on 0.2.5");
  expect(left({ outcome: "interrupted", stage: "restarting" })).toBe("Stopped partway through");
  expect(left({ outcome: "unknown", stage: "activating" })).toBe("Stopped reporting partway through");
});

it("says a server may not have upgraded when Ployz lost track of any attempt", () => {
  expect(mail(failure({ outcome: "unknown" }))).toMatchObject({ subject: "web-1 may not have upgraded to 0.2.6", title: "web-1 may not have upgraded" });
  expect(mail(failure({}), failure({ name: "web-2", outcome: "unknown" }))).toMatchObject({ subject: "2 servers may not have upgraded to 0.2.6" });
});

it("counts several servers and links the list", () => {
  const several = mail(failure({}), failure({ name: "web-2" }), failure({ name: "worker-1" }));
  expect(several).toMatchObject({
    subject: "3 servers didn't upgrade to 0.2.6",
    title: "3 servers didn't upgrade",
    action: { label: "View servers", url: "https://ployz.dev/cloud/acme/~/servers" },
  });
  expect(several.facts?.map((fact) => fact.label)).toEqual(["web-1", "web-2", "worker-1"]);
});
