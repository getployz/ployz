import { useState } from "react";
import { toast } from "sonner";
import { RelativeTime } from "#/components/relative-time";
import { Button } from "#/components/ui/button";
import type { ServerStatus } from "#/modules/machines/server-status";
import { newestAttempt, releaseLine, serversUpgradeLine, type ServersUpgradeLine } from "#/modules/server-upgrade/server-upgrade";
import { requestServerUpgradeServerFn } from "#/modules/server-upgrade/server-upgrade.functions";
import { useServerUpgrades, useStableRelease } from "#/modules/server-upgrade/server-upgrade.queries";
import { usePendingUpgrade, useNow } from "./server-upgrade-section";
import { ServerUpgradesDialog, useAutomaticUpgrades } from "./server-upgrades-dialog";

type LineServer = { readonly name: string; readonly version: string; readonly status: ServerStatus };

/** The Servers page's one upgrade line: which release the Servers run, a rollout's progress, or Upgrade. */
export function ServersUpgradeLine({ organizationSlug, servers }: { organizationSlug: string; servers: readonly LineServer[] }) {
  const upgrades = useServerUpgrades(organizationSlug);
  const { automatic } = useAutomaticUpgrades(organizationSlug);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // ponytail: Cloud never moves a Server across release lines, so the first Server that reports a version names the line.
  const release = useStableRelease(servers.map(({ version }) => releaseLine(version)).find((line) => line !== null) ?? null);
  const latest = Object.values(upgrades?.servers ?? {});
  const pending = usePendingUpgrade(newestAttempt(latest)?.attemptId ?? null);
  const now = useNow(latest.some((row) => row.outcome === "running"));

  // Nothing is offered before Cloud says what the last attempts did.
  const line = upgrades === undefined ? null : serversUpgradeLine({
    servers,
    release,
    latest,
    lastUpgradedAt: upgrades.lastUpgradedAt,
    pendingFrom: pending.from,
    now,
    automatic,
  });

  function upgrade() {
    pending.start();
    requestServerUpgradeServerFn({ data: { organizationSlug, machineId: null } }).catch(() => {
      pending.cancel();
      toast.error("Could not upgrade your servers");
    });
  }

  return (
    <>
      <ServersUpgradeText line={line} automatic={automatic} onUpgrade={upgrade} onSettings={() => setSettingsOpen(true)} />
      <ServerUpgradesDialog organizationSlug={organizationSlug} open={settingsOpen} onOpenChange={setSettingsOpen} />
    </>
  );
}

const list = new Intl.ListFormat("en", { type: "conjunction" });

/** What the upgrade line says for one state. */
export function ServersUpgradeText({ line, automatic, onUpgrade, onSettings }: {
  line: ServersUpgradeLine;
  automatic: boolean;
  onUpgrade: () => void;
  onSettings: () => void;
}) {
  if (line === null) return null;
  const mono = (version: string) => <span className="font-mono">{version}</span>;
  const settings = (
    <Button variant="link" size="xs" className="h-auto px-0" onClick={onSettings}>
      {automatic ? "Upgrades automatically" : "Manual upgrades"}
    </Button>
  );
  return (
    <div className="flex items-center gap-3">
      <p className="flex-1 text-sm text-muted-foreground">
        {line.kind === "current" ? (
          <>On Ployz {mono(line.release)}{line.upgradedAt === null ? null : <>, upgraded <RelativeTime date={new Date(line.upgradedAt)} /></>} · {settings}</>
        ) : line.kind === "when-back" ? (
          <>
            On Ployz {mono(line.release)} · {list.format(line.names)}
            {line.names.length === 1 ? " upgrades when it’s back" : " upgrade when they’re back"} · {settings}
          </>
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
        <>
          <Button variant="ghost" size="sm" onClick={onSettings}>{automatic ? "Automatic" : "Manual"}</Button>
          {line.canUpgrade ? (
            <Button variant="outline" size="sm" onClick={onUpgrade}>{line.upgraded > 0 ? "Upgrade the rest" : "Upgrade"}</Button>
          ) : null}
        </>
      ) : null}
    </div>
  );
}
