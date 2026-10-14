import { expect, it } from "vitest";
import { type UpgradeFailure, upgradeFailedMail } from "#/modules/mail/templates/servers/upgrade-failed";

const failure = (targetVersion: string | null): UpgradeFailure => ({
  machineId: "a".repeat(32), name: "web-1", fromVersion: "0.2.5", targetVersion, outcome: "failed", stage: null, error: null,
});
const paragraphs = (targetVersion: string | null) =>
  upgradeFailedMail({ organization: { name: "Acme", slug: "acme" }, failures: [failure(targetVersion)], origin: "https://ployz.dev" }).paragraphs;

it("says the release is skipped only when the attempt knew its release", () => {
  expect(paragraphs("0.2.6")).toEqual(["An automatic upgrade in Acme didn't finish. Automatic upgrades skip this release until an Upgrade of it succeeds."]);
  expect(paragraphs(null)).toEqual(["An automatic upgrade in Acme didn't finish."]);
});
