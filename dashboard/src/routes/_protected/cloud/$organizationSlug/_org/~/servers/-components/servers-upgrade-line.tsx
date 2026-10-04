import { useState } from "react";
import { toast } from "sonner";
import { RelativeTime } from "#/components/relative-time";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import type { ServerStatus } from "#/modules/machines/server-status";
import {
  newestAttempt,
  newMajorLine,
  releaseLine,
  serversUpgradeLine,
  type ServersUpgradeLine,
} from "#/modules/server-upgrade/server-upgrade";
import { requestServerUpgradeServerFn } from "#/modules/server-upgrade/server-upgrade.functions";
import { useChannelRelease, useServerUpgrades } from "#/modules/server-upgrade/server-upgrade.queries";
import { usePendingUpgrade, useNow } from "./server-upgrade-section";
import { ServerUpgradesDialog, useServerUpgradeSettings } from "./server-upgrades-dialog";

type LineServer = { readonly name: string; readonly version: string; readonly status: ServerStatus };

/** The Servers page's one upgrade line: which release the Servers run, a rollout's progress, or Upgrade. */
export function ServersUpgradeLine({ organizationSlug, servers }: { organizationSlug: string; servers: readonly LineServer[] }) {
  const upgrades = useServerUpgrades(organizationSlug);
  const { automatic, channel } = useServerUpgradeSettings(organizationSlug);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // ponytail: Cloud never moves a Server across release lines, so the first Server that reports a version names the line.
  const serversLine = servers.map(({ version }) => releaseLine(version)).find((line) => line !== null) ?? null;
  const release = useChannelRelease(channel, serversLine, serversLine !== null);
  // The installer's unscoped pointer: the only place a newer major line shows.
  const newest = useChannelRelease("stable", null, serversLine !== null);
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
      <NewMajorLineNotice notice={newMajorLine(serversLine, newest)} />
      <ServersUpgradeText line={line} automatic={automatic} onUpgrade={upgrade} onSettings={() => setSettingsOpen(true)} />
      <ServerUpgradesDialog organizationSlug={organizationSlug} open={settingsOpen} onOpenChange={setSettingsOpen} />
    </>
  );
}

const list = new Intl.ListFormat("en", { type: "conjunction" });

/** A newer major line is out: say so and link its release notes. Nothing installs it. */
export function NewMajorLineNotice({ notice }: { notice: ReturnType<typeof newMajorLine> }) {
  if (notice === null) return null;
  return (
    <div role="status" className="flex items-center gap-3 rounded-lg border bg-card px-3 py-2 text-sm">
      <p className="flex-1 text-muted-foreground">
        <span className="font-medium text-foreground">Ployz {notice.name} is out.</span> New lines install only when you
        choose. Your servers stay on {notice.running} and keep getting its upgrades.
      </p>
      <a
        href={`https://github.com/getployz/ployz/releases/tag/v${notice.release}`}
        target="_blank"
        rel="noreferrer"
        className={buttonVariants({ variant: "outline", size: "sm" })}
      >
        See what’s new
      </a>
    </div>
  );
}

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
