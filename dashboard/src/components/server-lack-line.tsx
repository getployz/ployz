import { type ReactNode, Suspense, useEffect } from "react";
import { useQueryErrorResetBoundary } from "@tanstack/react-query";
import { useLiveQuery } from "@tanstack/react-db";
import { CatchBoundary } from "@tanstack/react-router";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Button } from "#/components/ui/button";
import { serverStatus } from "#/modules/machines/server-status";
import { requirementFor, type ServerLacks } from "#/modules/runtime/daemon-compat";
import { getRuntimeCollections, type RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { releaseLine, remedy, rolloutRunning, serverUpgradeLine, type Remedy } from "#/modules/server-upgrade/server-upgrade";
import { useNow, useRequestUpgrade, useServerUpgradeSettings } from "#/modules/server-upgrade/server-upgrade.hooks";
import { useChannelRelease, useServerUpgrades } from "#/modules/server-upgrade/server-upgrade.queries";

export function ServerLackLine({ organizationSlug, lack, name }: { organizationSlug: string; lack: ServerLacks; name: string }) {
  const { data: machines = [] } = useLiveQuery(getRuntimeCollections(organizationSlug, useCollectionScope()).machines);
  const machine = machines.find((row) => row.id === lack.machineId);
  const feature = requirementFor(lack.capability);
  const unread = <LackLine server={machine?.name ?? name} remedy={remedy({
    feature, server: { name: machine?.name ?? name, daemonVersion: machine?.daemonVersion ?? "" }, release: { state: "pending" }, line: null,
  })} onUpgrade={() => {}} />;
  return <CatchBoundary getResetKey={() => lack.machineId} errorComponent={({ reset }) => <Retrying reset={reset}>{unread}</Retrying>}>
    <Suspense fallback={unread}>{machine ? <Upgradable organizationSlug={organizationSlug} machine={machine} capability={lack.capability} /> : unread}</Suspense>
  </CatchBoundary>;
}

/** A read that failed under the line tries again after a while, so its release and button come back without a reload. */
const RETRY_MS = 10_000;

function Retrying({ reset, children }: { reset: () => void; children: ReactNode }) {
  const queries = useQueryErrorResetBoundary();
  useEffect(() => {
    const timer = setTimeout(() => { queries.reset(); reset(); }, RETRY_MS);
    return () => clearTimeout(timer);
  }, [queries, reset]);
  return children;
}

function Upgradable({ organizationSlug, machine, capability }: { organizationSlug: string; machine: RuntimeMachineRecord; capability: string }) {
  const upgrades = useServerUpgrades(organizationSlug);
  const latest = upgrades.servers[machine.id] ?? null;
  const everyLatest = Object.values(upgrades.servers);
  const { automatic, channel } = useServerUpgradeSettings(organizationSlug);
  const serverLine = releaseLine(machine.daemonVersion);
  const release = useChannelRelease(serverLine === null ? null : { channel, line: serverLine });
  const { pendingFrom, upgrade } = useRequestUpgrade(organizationSlug, latest?.attemptId ?? null, machine);
  const now = useNow(everyLatest.some((row) => row.outcome === "running"));
  const line = serverUpgradeLine({
    version: machine.daemonVersion,
    status: serverStatus(machine),
    release: release.isSuccess ? release.data : null,
    latest,
    pendingFrom,
    now,
    automatic,
    rolloutRunning: rolloutRunning(everyLatest, now),
  });
  return <LackLine server={machine.name} onUpgrade={upgrade} remedy={remedy({
    feature: requirementFor(capability),
    server: { name: machine.name, daemonVersion: machine.daemonVersion },
    release: release.isPending ? { state: "pending" } : release.isError ? { state: "failed" } : { state: "resolved", release: release.data ?? null },
    line,
  })} />;
}

export function LackLine({ server, remedy, onUpgrade }: { server: string; remedy: Remedy; onUpgrade: () => void }) {
  return <div role="status" className="flex flex-wrap items-center gap-x-3 gap-y-1">
    <span>
      {remedy.kind === "behind" || remedy.kind === "lack" ? remedy.sentence
        : remedy.kind === "upgrading" ? <>Upgrading {server}{remedy.target === null ? null : <> to <span className="font-mono">{remedy.target}</span></>}</>
        : remedy.kind === "when-back" ? <>{server} upgrades to <span className="font-mono">{remedy.release}</span> when it’s back</>
        : <><span className="font-mono">{remedy.target}</span> didn’t install on {server}, and we’re on it.</>}
    </span>
    {remedy.kind === "behind" && remedy.canUpgrade ? <Button variant="outline" size="sm" onClick={onUpgrade}>Upgrade</Button>
      : remedy.kind === "failed" && remedy.canRetry ? <Button variant="outline" size="sm" onClick={onUpgrade}>Try again</Button>
      : null}
  </div>;
}
