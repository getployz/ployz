import { useEffect, useState } from "react";
import { toast } from "sonner";
import { CopyButton } from "#/components/copy-button";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import type { ServerStatus } from "#/modules/machines/server-status";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { releaseLine, rolloutRunning, serverUpgradeLine, type ServerUpgradeLine } from "#/modules/server-upgrade/server-upgrade";
import { requestServerUpgradeServerFn } from "#/modules/server-upgrade/server-upgrade.functions";
import { useChannelRelease, useServerUpgrades } from "#/modules/server-upgrade/server-upgrade.queries";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { useServerUpgradeSettings } from "./server-upgrades-dialog";

/** How long a click reads as Upgrading before its attempt is recorded; a Busy Server records none. */
const PENDING_MS = 60_000;

/**
 * A click on Upgrade reads as Upgrading until a newer attempt than `newestAttemptId` is recorded, or for a minute: a
 * Busy Server records none. `from` is the newest attempt ID at the click; undefined when nothing is pending.
 */
export function usePendingUpgrade(newestAttemptId: string | null) {
  const [from, setFrom] = useState<string | null | undefined>(undefined);
  const recorded = from !== undefined && newestAttemptId !== from;
  useEffect(() => {
    if (recorded) setFrom(undefined);
  }, [recorded]);
  useEffect(() => {
    if (from === undefined) return;
    const timer = setTimeout(() => setFrom(undefined), PENDING_MS);
    return () => clearTimeout(timer);
  }, [from]);
  return { from, start: () => setFrom(newestAttemptId), cancel: () => setFrom(undefined) };
}

/** The clock, ticking while an attempt runs: one reads as unknown once it outlives the observation limit. */
export function useNow(running: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, [running]);
  return now;
}

/** The Ployz release this Server runs, and its one Upgrade line. */
export function ServerUpgradeSection({ machine, status, organizationSlug }: {
  machine: RuntimeMachineRecord;
  status: ServerStatus;
  organizationSlug: string;
}) {
  const version = machine.daemonVersion;
  const upgrades = useServerUpgrades(organizationSlug);
  const latest = upgrades === undefined ? undefined : upgrades.servers[machine.id] ?? null;
  const everyLatest = Object.values(upgrades?.servers ?? {});
  const { automatic, channel } = useServerUpgradeSettings(organizationSlug);
  const serverLine = releaseLine(version);
  const release = useChannelRelease(channel, serverLine, serverLine !== null);
  const pending = usePendingUpgrade(latest?.attemptId ?? null);
  const now = useNow(everyLatest.some((row) => row.outcome === "running"));

  // Nothing is offered before Cloud says what the last attempt did.
  const line = latest === undefined ? null : serverUpgradeLine({
    version, status, release, latest, pendingFrom: pending.from, now, automatic, rolloutRunning: rolloutRunning(everyLatest, now),
  });

  function upgrade() {
    pending.start();
    requestServerUpgradeServerFn({ data: { organizationSlug, machineId: machine.id } }).catch(() => {
      pending.cancel();
      toast.error(`Could not upgrade ${machine.name}`);
    });
  }

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
                      <Button variant="link" size="xs" className="h-auto px-0" onClick={() => setShowDetails(!showDetails)}>
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
