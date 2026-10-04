import { toast } from "sonner";
import { RelativeTime } from "#/components/relative-time";
import { Button } from "#/components/ui/button";
import { newestAttempt, releaseLine, serversUpgradeLine, type ServersUpgradeLine } from "#/modules/server-upgrade/server-upgrade";
import { requestServerUpgradeServerFn } from "#/modules/server-upgrade/server-upgrade.functions";
import { useServerUpgrades, useStableRelease } from "#/modules/server-upgrade/server-upgrade.queries";
import { usePendingUpgrade, useNow } from "./server-upgrade-section";

/** The Servers page's one upgrade line: which release the Servers run, a rollout's progress, or Upgrade. */
export function ServersUpgradeLine({ organizationSlug, versions }: { organizationSlug: string; versions: readonly string[] }) {
  const upgrades = useServerUpgrades(organizationSlug);
  // ponytail: Cloud never moves a Server across release lines, so the first Server that reports a version names the line.
  const release = useStableRelease(versions.map(releaseLine).find((line) => line !== null) ?? null);
  const latest = Object.values(upgrades?.servers ?? {});
  const pending = usePendingUpgrade(newestAttempt(latest)?.attemptId ?? null);
  const now = useNow(latest.some((row) => row.outcome === "running"));

  // Nothing is offered before Cloud says what the last attempts did.
  const line = upgrades === undefined ? null : serversUpgradeLine({
    servers: versions.map((version) => ({ version })),
    release,
    latest,
    lastUpgradedAt: upgrades.lastUpgradedAt,
    pendingFrom: pending.from,
    now,
  });

  function upgrade() {
    pending.start();
    requestServerUpgradeServerFn({ data: { organizationSlug, machineId: null } }).catch(() => {
      pending.cancel();
      toast.error("Could not upgrade your servers");
    });
  }

  return <ServersUpgradeText line={line} onUpgrade={upgrade} />;
}

/** What the upgrade line says for one state. */
export function ServersUpgradeText({ line, onUpgrade }: { line: ServersUpgradeLine; onUpgrade: () => void }) {
  if (line === null) return null;
  const mono = (version: string) => <span className="font-mono">{version}</span>;
  return (
    <div className="flex items-center gap-3">
      <p className="flex-1 text-sm text-muted-foreground">
        {line.kind === "current" ? (
          <>On Ployz {mono(line.release)}{line.upgradedAt === null ? null : <>, upgraded <RelativeTime date={new Date(line.upgradedAt)} /></>}</>
        ) : line.kind === "upgrading" ? (
          <>Upgrading to {mono(line.target)} · {line.done} of {line.total} done</>
        ) : (
          <>
            Ployz {mono(line.release)} is out
            {line.upgraded > 0 ? <> · {line.upgraded} of {line.total} upgraded</>
              : line.running === null ? null : <> · your servers run {mono(line.running)}</>}
          </>
        )}
      </p>
      {line.kind === "behind" ? (
        <Button variant="outline" size="sm" onClick={onUpgrade}>{line.upgraded > 0 ? "Upgrade the rest" : "Upgrade"}</Button>
      ) : null}
    </div>
  );
}
