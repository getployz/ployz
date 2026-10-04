import { useState } from "react";
import { CopyButton } from "#/components/copy-button";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import type { ServerStatus } from "#/modules/machines/server-status";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { releaseLine, rolloutRunning, serverUpgradeLine, type ServerUpgradeLine } from "#/modules/server-upgrade/server-upgrade";
import { useNow, useRequestUpgrade, useServerUpgradeSettings } from "#/modules/server-upgrade/server-upgrade.hooks";
import { useChannelRelease, useServerUpgrades } from "#/modules/server-upgrade/server-upgrade.queries";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

/** The Ployz release this Server runs, and its one Upgrade line. */
export function ServerUpgradeSection({ machine, status, organizationSlug }: {
  machine: RuntimeMachineRecord;
  status: ServerStatus;
  organizationSlug: string;
}) {
  const version = machine.daemonVersion;
  const upgrades = useServerUpgrades(organizationSlug);
  const latest = upgrades.servers[machine.id] ?? null;
  const everyLatest = Object.values(upgrades.servers);
  const { automatic, channel } = useServerUpgradeSettings(organizationSlug);
  const serverLine = releaseLine(version);
  const release = useChannelRelease(serverLine === null ? null : { channel, line: serverLine });
  const { pendingFrom, upgrade } = useRequestUpgrade(organizationSlug, latest?.attemptId ?? null, machine);
  const now = useNow(everyLatest.some((row) => row.outcome === "running"));

  const line = serverUpgradeLine({
    version,
    status,
    // Until the release is read, or when it can't be, nothing new is offered; the latest attempt still shows.
    release: release.isSuccess ? release.data : null,
    latest,
    pendingFrom,
    now,
    automatic,
    rolloutRunning: rolloutRunning(everyLatest, now),
  });

  return <ServerUpgradeRow version={version} line={line} onUpgrade={upgrade} />;
}

/** What the Ployz section shows for one Upgrade line. */
export function ServerUpgradeRow({ version, line, onUpgrade }: {
  version: string;
  line: ServerUpgradeLine;
  onUpgrade: () => void;
}) {
  const [showDetails, setShowDetails] = useState(false);
  return (
    <SettingsSection id="server-ployz" title="Ployz">
      <Field orientation="horizontal">
        <FieldContent>
          <FieldLabel className="font-mono">{version || "Version unknown"}</FieldLabel>
          {line === null ? null : (
            <FieldDescription>
              {line.kind === "behind" ? (
                <><span className="font-mono">{line.release}</span> is out</>
              ) : line.kind === "when-back" ? (
                <>Upgrades to <span className="font-mono">{line.release}</span> when it’s back</>
              ) : line.kind === "upgrading" ? (
                <>Upgrading{line.target === null ? null : <> to <span className="font-mono">{line.target}</span></>}</>
              ) : (
                <>
                  <span className="font-mono">{line.target}</span>
                  {line.nothingChanged ? " didn’t install. Nothing changed, and we’re on it." : " didn’t install, and we’re on it."}
                  {line.details === null ? null : (
                    <>
                      {" "}
                      <Button variant="link" size="xs" onClick={() => setShowDetails(!showDetails)}>
                        {showDetails ? "Hide details" : "Show details"}
                      </Button>
                    </>
                  )}
                </>
              )}
            </FieldDescription>
          )}
        </FieldContent>
        {line?.kind === "behind" && line.canUpgrade ? (
          <Button variant="outline" onClick={onUpgrade}>Upgrade</Button>
        ) : line?.kind === "failed" && line.canRetry ? (
          <Button variant="outline" onClick={() => { setShowDetails(false); onUpgrade(); }}>Try again</Button>
        ) : null}
      </Field>
      {line?.kind === "failed" && line.details !== null && showDetails ? (
        <Field orientation="horizontal">
          <pre className="min-w-0 flex-1 font-mono text-sm whitespace-pre-wrap text-muted-foreground">{line.details}</pre>
          <CopyButton value={line.details} label="Copy" showLabel variant="outline" />
        </Field>
      ) : null}
    </SettingsSection>
  );
}
