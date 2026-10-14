import type { MailContent } from "#/modules/mail/layout";
import type { FinalOutcome } from "#/modules/server-upgrade/server-upgrade";

export type UpgradeFailure = {
  readonly machineId: string;
  readonly name: string;
  /** Empty when the attempt predates daemon versions. */
  readonly fromVersion: string;
  readonly targetVersion: string | null;
  readonly outcome: Exclude<FinalOutcome, "succeeded">;
  readonly stage: string | null;
  readonly error: string | null;
};

export type UpgradeFailedMail = {
  readonly organization: { readonly name: string; readonly slug: string };
  readonly failures: readonly [UpgradeFailure, ...UpgradeFailure[]];
  readonly origin: string;
};

const versions = ({ fromVersion, targetVersion }: UpgradeFailure) => {
  if (fromVersion !== "" && targetVersion !== null) return `${fromVersion} to ${targetVersion}`;
  if (targetVersion !== null) return `To ${targetVersion}`;
  return fromVersion === "" ? null : `From ${fromVersion}`;
};

const ending = ({ outcome, stage, error }: UpgradeFailure) => {
  const at = stage === null ? "" : ` at ${stage}`;
  switch (outcome) {
    case "failed":
      return error === null ? `Failed${at}` : `Failed${at}: ${error}`;
    case "interrupted":
      return `Interrupted${at}`;
    case "unknown":
      return stage === null ? "Outcome unknown" : `Outcome unknown, last seen at ${stage}`;
  }
};

/** One email per automatic Rollout that didn't finish, naming every Server whose attempt didn't succeed. */
export function upgradeFailedMail({ organization, failures, origin }: UpgradeFailedMail): Omit<MailContent, "unsubscribe"> {
  const [first] = failures;
  const one = failures.length === 1;
  const servers = one ? first.name : `${failures.length} Servers`;
  const target = first.targetVersion === null ? "" : ` to ${first.targetVersion}`;
  return {
    subject: `Automatic upgrade failed on ${servers}`,
    preview: `${first.name} didn't upgrade${target}.`,
    title: `${servers} didn't upgrade`,
    paragraphs: [
      `An automatic upgrade in ${organization.name} didn't finish. Automatic upgrades skip this release until an Upgrade of it succeeds.`,
    ],
    action: one
      ? { label: "View Server", url: `${origin}/cloud/${organization.slug}/~/servers/${first.machineId}` }
      : { label: "View Servers", url: `${origin}/cloud/${organization.slug}/~/servers` },
    facts: failures.map((failure) => ({
      label: failure.name,
      value: [versions(failure), ending(failure)].filter((part) => part !== null).join(". "),
    })),
    reason: `You're getting this because you're a member of ${organization.name} on Ployz.`,
  };
}

const sampleFailure = (machineId: string, name: string, overrides: Partial<UpgradeFailure>): UpgradeFailure => ({
  machineId,
  name,
  fromVersion: "0.2.5",
  targetVersion: "0.2.6",
  outcome: "failed",
  stage: "readiness",
  error: "readiness timed out; restored 0.2.5",
  ...overrides,
});

/** Previews: the usual one Server, and three to show every ending. */
export const upgradeFailedSamples = (origin: string) => ({
  "upgrade-failed.one": upgradeFailedMail({
    organization: { name: "Acme", slug: "acme" },
    failures: [sampleFailure("a".repeat(32), "web-1", {})],
    origin,
  }),
  "upgrade-failed.three": upgradeFailedMail({
    organization: { name: "Acme", slug: "acme" },
    failures: [
      sampleFailure("a".repeat(32), "web-1", {}),
      sampleFailure("b".repeat(32), "web-2", { outcome: "interrupted", stage: "restarting", error: null }),
      sampleFailure("c".repeat(32), "worker-1", { outcome: "unknown", stage: "activating", error: null }),
    ],
    origin,
  }),
});
