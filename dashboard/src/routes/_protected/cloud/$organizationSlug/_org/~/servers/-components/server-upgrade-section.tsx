import { useEffect, useState } from "react";
import { toast } from "sonner";
import { CopyButton } from "#/components/copy-button";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import type { ServerStatus } from "#/modules/machines/server-status";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { releaseLine, serverUpgradeLine } from "#/modules/server-upgrade/server-upgrade";
import { requestServerUpgradeServerFn } from "#/modules/server-upgrade/server-upgrade.functions";
import { useLatestServerUpgrade, useStableRelease } from "#/modules/server-upgrade/server-upgrade.queries";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

/** How long a click reads as Upgrading before its attempt is recorded; a Busy Server records none. */
const PENDING_MS = 60_000;

/** The Ployz release this Server runs, and its one Upgrade line. */
export function ServerUpgradeSection({ machine, status, organizationSlug }: {
  machine: RuntimeMachineRecord;
  status: ServerStatus;
  organizationSlug: string;
}) {
  const version = machine.daemonVersion;
  const latest = useLatestServerUpgrade(organizationSlug, machine.id);
  const release = useStableRelease(releaseLine(version));
  // The latest attempt ID when Upgrade was clicked; undefined when nothing is pending.
  const [pendingFrom, setPendingFrom] = useState<string | null | undefined>(undefined);
  const [showDetails, setShowDetails] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  const recorded = pendingFrom !== undefined && (latest?.attemptId ?? null) !== pendingFrom;
  useEffect(() => {
    if (recorded) setPendingFrom(undefined);
  }, [recorded]);
  useEffect(() => {
    if (pendingFrom === undefined) return;
    const timer = setTimeout(() => setPendingFrom(undefined), PENDING_MS);
    return () => clearTimeout(timer);
  }, [pendingFrom]);
  // A running attempt reads as unknown once it outlives the observation limit.
  useEffect(() => {
    if (latest?.outcome !== "running") return;
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, [latest?.outcome]);

  // Nothing is offered before Cloud says what the last attempt did.
  const line = latest === undefined ? null : serverUpgradeLine({ version, status, release, latest, pendingFrom, now });

  function upgrade() {
    setPendingFrom(latest?.attemptId ?? null);
    setShowDetails(false);
    requestServerUpgradeServerFn({ data: { organizationSlug, machineId: machine.id } }).catch(() => {
      setPendingFrom(undefined);
      toast.error(`Could not upgrade ${machine.name}`);
    });
  }

  return (
    <SettingsSection id="server-ployz" title="Ployz">
      <Field orientation="horizontal">
        <FieldContent>
          <FieldLabel className="font-mono">{version || "Version unknown"}</FieldLabel>
          {line === null ? null : (
            <FieldDescription>
              {line.kind === "behind" ? (
                <><span className="font-mono">{line.release}</span> is out</>
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
        {line?.kind === "behind" ? (
          <Button variant="outline" onClick={upgrade}>Upgrade</Button>
        ) : line?.kind === "failed" && line.canRetry ? (
          <Button variant="outline" onClick={upgrade}>Try again</Button>
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
