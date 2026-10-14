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

/** Stages at and after the swap: a stop here may leave the new release half in place. */
const AFTER_SWAP = new Set(["activating", "restarting", "readiness"]);

/** Where the Server was left, in the daemon's own terms: its failure text ends `; restored <v>` or `; restore failed: …`. */
const whereLeft = ({ fromVersion, outcome, stage, error }: UpgradeFailure) => {
  const unchanged = fromVersion === "" ? "Nothing changed" : `Still on ${fromVersion}`;
  const swapped = stage !== null && AFTER_SWAP.has(stage);
  switch (outcome) {
    case "failed": {
      const restored = error === null ? null : /; restored (\S+)$/.exec(error);
      if (restored) return `Rolled back to ${restored[1]}`;
      if (error?.includes("; restore failed:")) return "Couldn't roll back";
      return unchanged;
    }
    case "interrupted":
      return swapped ? "Stopped partway through" : unchanged;
    case "unknown":
      return "Stopped reporting partway through";
  }
};

/** One email per automatic Rollout that didn't finish, naming every Server whose attempt didn't succeed. */
export function upgradeFailedMail({ organization, failures, origin }: UpgradeFailedMail): Omit<MailContent, "unsubscribe"> {
  const [first] = failures;
  const one = failures.length === 1;
  const servers = one ? first.name : `${failures.length} servers`;
  const release = failures.find((failure) => failure.targetVersion !== null)?.targetVersion ?? null;
  const skip = release === null ? null : `Automatic upgrades skip ${release} until a newer release is out or you upgrade a server to it yourself.`;
  return {
    subject: release === null ? `${servers} didn't upgrade` : `${servers} didn't upgrade to ${release}`,
    preview: one ? `${whereLeft(first)}.` : `Ployz stopped the automatic upgrade in ${organization.name}.`,
    title: `${servers} didn't upgrade`,
    paragraphs: [[`Ployz stopped the automatic upgrade in ${organization.name}.`, skip].filter((line) => line !== null).join(" ")],
    action: one
      ? { label: "View server", url: `${origin}/cloud/${organization.slug}/~/servers/${first.machineId}` }
      : { label: "View servers", url: `${origin}/cloud/${organization.slug}/~/servers` },
    facts: failures.map((failure) => ({ label: failure.name, value: whereLeft(failure) })),
    reason: `You're getting this as a member of ${organization.name} on Ployz.`,
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
