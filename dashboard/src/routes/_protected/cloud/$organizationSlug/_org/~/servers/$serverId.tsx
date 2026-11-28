import { Suspense } from "react";
import { createFileRoute } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";
import { TriangleAlertIcon } from "lucide-react";
import { CopyButton } from "#/components/copy-button";
import { DashboardPage } from "#/components/dashboard-page";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { Skeleton } from "#/components/ui/skeleton";
import { useServers } from "#/modules/machines/use-servers";
import { latestServerUpgradesQueryOptions } from "#/modules/server-upgrade/server-upgrade.queries";
import { organizationMembersQueryOptions } from "#/modules/organization/members.queries";
import { serverDrains } from "#/modules/machines/server-drain.queries";
import { serverWebTraffic } from "#/modules/machines/server-web-traffic.queries";
import { volumePlacementsQueryOptions } from "#/modules/volume-placement/volume-placement.queries";
import { prefetchRemote } from "#/collections/route-data";
import { RunningHere } from "./-components/running-here";
import { ServerLeftoversSection } from "./-components/server-leftovers-section";
import { ServerResources } from "./-components/server-resources";
import { ServerSettings } from "./-components/server-settings";
import { ServerStatusLabel } from "./-components/server-status-label";
import { ServerSwitcher } from "./-components/server-switcher";
import { ServerTrashSection } from "./-components/server-trash-section";
import { ServerUpgradeSection } from "./-components/server-upgrade-section";
import { ServersStaleAlert, ServersUnreachable } from "./-components/servers-unreachable";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/$serverId",
)({
  // `services` opens the Services on this Server in a sheet, so a long list never pushes the settings down.
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({
    services: Schema.optional(Schema.Boolean.pipe(
      Schema.catchDecoding(() => Effect.succeed(Option.some(false))),
    )),
  })),
  staticData: { crumb: ServerSwitcher, section: "servers" },
  loader: ({ params, context }) => prefetchRemote(
    context,
    latestServerUpgradesQueryOptions(params.organizationSlug),
    serverDrains.options(params.organizationSlug),
    serverWebTraffic.options(params.organizationSlug),
    volumePlacementsQueryOptions(params.organizationSlug),
    organizationMembersQueryOptions(params.organizationSlug),
  ),
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, serverId } = Route.useParams();
  const { state, servers, unreachableReason } = useServers(organizationSlug);
  const server = servers.find((candidate) => candidate.machine.id === serverId);

  if (state === "loading") {
    return <DashboardPage width="content" title="Server"><ServerPageSkeleton /></DashboardPage>;
  }
  if (state === "unreachable" || (state === "stale" && !server)) {
    return <DashboardPage width="content" title="Server"><ServersUnreachable organizationSlug={organizationSlug} reason={unreachableReason} /></DashboardPage>;
  }
  if (!server) {
    return (
      <DashboardPage width="content" title="Server">
        <Empty variant="first-run">
          <EmptyHeader>
            <EmptyTitle>This server is no longer in your organization</EmptyTitle>
            <EmptyDescription>It may have been removed.</EmptyDescription>
          </EmptyHeader>
        </Empty>
      </DashboardPage>
    );
  }

  const stale = state === "stale";
  const { machine } = server;
  return (
    <DashboardPage width="content" title={server.name}>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm">
        <ServerStatusLabel status={server.status} stale={stale} />
        {machine.publicIp ? (
          <span className="inline-flex items-center gap-1 font-mono text-muted-foreground">
            {machine.publicIp}
            <CopyButton value={machine.publicIp} label="Copy address" size="icon-xs" variant="ghost" />
          </span>
        ) : null}
      </div>
      {stale ? (
        <ServersStaleAlert />
      ) : server.status === "offline" ? (
        <Alert variant="destructive">
          <TriangleAlertIcon />
          <AlertTitle>Can’t reach {server.name}</AlertTitle>
          <AlertDescription>Check that it’s powered on and online.</AlertDescription>
        </Alert>
      ) : null}
      {server.status === "offline" ? null : <ServerResources organizationSlug={organizationSlug} server={{ id: machine.id, name: server.name, version: machine.daemonVersion }} />}
      <RunningHere organizationSlug={organizationSlug} server={server} servers={servers} stale={stale} />
      <Suspense fallback={null}>
        <ServerUpgradeSection machine={machine} status={server.status} organizationSlug={organizationSlug} />
      </Suspense>
      {server.status === "offline" ? null : <ServerLeftoversSection organizationSlug={organizationSlug} server={{ id: machine.id, name: server.name }} />}
      {server.status === "offline" ? null : <ServerTrashSection organizationSlug={organizationSlug} server={{ id: machine.id, name: server.name }} />}
      <Suspense fallback={null}>
        <ServerSettings organizationSlug={organizationSlug} server={server} servers={servers} stale={stale} />
      </Suspense>
    </DashboardPage>
  );
}

/** The page's shape while the Runtime Watch connects: status, Resources, Running here, Services, Builds, Remove. */
function ServerPageSkeleton() {
  return (
    <div role="status" aria-label="Server loading" className="flex flex-col gap-6">
      <Skeleton className="h-5 w-48" />
      {[3, 3, 2, 2, 1].map((rows, section) => (
        <div key={section} aria-hidden="true" className="flex flex-col gap-2">
          <Skeleton className="h-4 w-24" />
          {Array.from({ length: rows }, (_, row) => <Skeleton key={row} className="h-14 w-full rounded-lg" />)}
        </div>
      ))}
    </div>
  );
}
