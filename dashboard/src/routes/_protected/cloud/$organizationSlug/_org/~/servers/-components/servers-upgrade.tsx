import { useState } from "react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import type { ServerListItem } from "#/modules/machines/use-servers";
import {
  NEWEST_RELEASE,
  newestAttempt,
  newMajorLine,
  releaseLine,
  serversUpgradeLine,
  serverUpgradeLines,
  type ServersUpgradeLine,
  type ServerUpgradeLine,
} from "#/modules/server-upgrade/server-upgrade";
import { useNow, useRequestUpgrade, useServerUpgradeSettings } from "#/modules/server-upgrade/server-upgrade.hooks";
import { useChannelRelease, useServerUpgrades } from "#/modules/server-upgrade/server-upgrade.queries";
import { ServerUpgradesDialog } from "./server-upgrades-dialog";

/** What the Servers page shows about upgrades: the top bar's line, a newer major line, and each row's line by Machine ID. */
export function useServersUpgrade(organizationSlug: string, list: readonly ServerListItem[]) {
  const upgrades = useServerUpgrades(organizationSlug);
  const { automatic, channel } = useServerUpgradeSettings(organizationSlug);
  const servers = list.map(({ machine, status }) => ({ id: machine.id, version: machine.daemonVersion, status }));
  // ponytail: Cloud never moves a Server across release lines, so the first Server that reports a version names the line.
  const serversLine = servers.map(({ version }) => releaseLine(version)).find((line) => line !== null) ?? null;
  const release = useChannelRelease(serversLine === null ? null : { channel, line: serversLine });
  const newest = useChannelRelease(NEWEST_RELEASE);
  const latest = Object.values(upgrades.servers);
  const { pendingFrom, upgrade } = useRequestUpgrade(organizationSlug, newestAttempt(latest)?.attemptId ?? null, null);
  const now = useNow(latest.some((row) => row.outcome === "running"));
  // Until the release is read, or when it can't be, nothing new is offered; attempts still show on their rows.
  const known = release.isSuccess ? release.data : null;

  return {
    line: serversUpgradeLine({ servers, release: known, latest, pendingFrom, now, automatic }),
    rows: serverUpgradeLines({ servers, release: known, latest: upgrades.servers, pendingFrom, now, automatic }),
    major: newMajorLine(serversLine, newest.isSuccess ? newest.data : null),
    automatic,
    upgrade,
  };
}

export type ServersUpgrade = ReturnType<typeof useServersUpgrade>;

/** The top bar's upgrade controls, and the Server upgrades dialog they open. */
export function ServersUpgradeBar({ organizationSlug, upgrade }: { organizationSlug: string; upgrade: ServersUpgrade }) {
  const [settingsOpen, setSettingsOpen] = useState(false);
  return (
    <>
      <ServersUpgradeActions {...upgrade} onSettings={() => setSettingsOpen(true)} />
      <ServerUpgradesDialog organizationSlug={organizationSlug} open={settingsOpen} onOpenChange={setSettingsOpen} />
    </>
  );
}

const mono = (version: string) => <span className="font-mono">{version}</span>;

/** A newer major line's release notes, a rollout's progress or Upgrade, and the upgrade settings. */
export function ServersUpgradeActions({ line, major, automatic, upgrade, onSettings }: {
  line: ServersUpgradeLine;
  major: ReturnType<typeof newMajorLine>;
  automatic: boolean;
  upgrade: () => void;
  onSettings: () => void;
}) {
  return (
    <>
      {major === null ? null : (
        <a
          href={`https://github.com/getployz/ployz/releases/tag/v${major.release}`}
          target="_blank"
          rel="noreferrer"
          className={buttonVariants({ variant: "link" })}
        >
          Ployz {major.name} is out
        </a>
      )}
      {line?.kind === "upgrading" ? (
        <p className="text-sm text-muted-foreground">Upgrading to {mono(line.target)} · {line.done} of {line.total}</p>
      ) : line?.kind === "behind" && line.canUpgrade ? (
        <Button variant="outline" onClick={upgrade}>
          {line.upgraded > 0 ? "Upgrade the rest" : <>Upgrade to {mono(line.release)}</>}
        </Button>
      ) : null}
      <Button variant="ghost" onClick={onSettings}>Upgrades: {automatic ? "Automatic" : "Manual"}</Button>
    </>
  );
}

/** A Server row's upgrade state, beside its status. */
export function ServerUpgradeTag({ line }: { line: ServerUpgradeLine }) {
  if (line === null) return null;
  switch (line.kind) {
    case "upgrading": return <Badge variant="info">Upgrading</Badge>;
    case "failed": return <Badge variant="warning">Upgrade failed</Badge>;
    case "behind": return <Badge variant="outline">→ {mono(line.release)}</Badge>;
    case "when-back": return <Badge variant="outline">→ {mono(line.release)} when back</Badge>;
  }
}
