import { useState } from "react";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";
import { BoxIcon, ChevronRightIcon, TriangleAlertIcon } from "lucide-react";
import { CopyButton } from "#/components/copy-button";
import { DashboardPage } from "#/components/dashboard-page";
import { ServerStatusLabel } from "#/components/server-status-label";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { Input } from "#/components/ui/input";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Sheet, SheetContent, SheetHeader, SheetTitle } from "#/components/ui/sheet";
import { Skeleton } from "#/components/ui/skeleton";
import { needsAttention } from "#/modules/machines/server-status";
import { useServers, type Server } from "#/modules/machines/use-servers";
import { RemoveServerSection } from "./-components/remove-server-section";
import { runsHere } from "./-components/runs-here";
import { ServerBuildsSection } from "./-components/server-builds-section";
import { ServerSwitcher } from "./-components/server-switcher";
import { StrayNamespaces } from "./-components/stray-namespaces";
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
  staticData: { crumb: ServerSwitcher },
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, serverId } = Route.useParams();
  const { state, servers } = useServers(organizationSlug);
  const server = servers.find((candidate) => candidate.machine.id === serverId);

  if (state === "loading") {
    return <DashboardPage width="content"><ServerPageSkeleton /></DashboardPage>;
  }
  if (state === "unreachable" || (state === "stale" && !server)) {
    return <DashboardPage width="content"><ServersUnreachable organizationSlug={organizationSlug} /></DashboardPage>;
  }
  if (!server) {
    return (
      <DashboardPage width="content">
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
    <DashboardPage width="content">
      {/* The top bar's crumb names the server. */}
      <h1 className="sr-only">{server.name}</h1>
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
      <RunningHere organizationSlug={organizationSlug} server={server} servers={servers} stale={stale} />
      <ServerBuildsSection machine={machine} organizationSlug={organizationSlug} />
      <RemoveServerSection machine={machine} organizationSlug={organizationSlug} last={servers.length === 1} />
    </DashboardPage>
  );
}

/** The page's shape while the Runtime Watch connects: status, Running here, Builds, Remove. */
function ServerPageSkeleton() {
  return (
    <div role="status" aria-label="Server loading" className="flex flex-col gap-6">
      <Skeleton className="h-5 w-48" />
      {[1, 2, 1].map((rows, section) => (
        <div key={section} aria-hidden="true" className="flex flex-col gap-2">
          <Skeleton className="h-4 w-24" />
          {Array.from({ length: rows }, (_, row) => <Skeleton key={row} className="h-14 w-full rounded-lg" />)}
        </div>
      ))}
    </div>
  );
}

/**
 * How many Services run on this Server; the list itself opens in a sheet. While the Server is offline, the row says
 * how many run nowhere else, since that is what the outage takes down.
 */
function RunningHere({ organizationSlug, server, servers, stale }: {
  organizationSlug: string;
  server: Server;
  servers: readonly Server[];
  stale: boolean;
}) {
  const open = Route.useSearch().services === true;
  const navigate = useNavigate({ from: Route.fullPath });
  const up = servers.filter((other) => other.machine.id !== server.machine.id && !needsAttention(other.status));
  const count = server.services.length;
  const onlyHere = server.status === "offline"
    ? server.services.filter((service) => !up.some((other) => service.machineIds.has(other.machine.id))).length
    : 0;

  return (
    <section aria-labelledby="running-here-heading">
      <ItemGroup>
        <ItemContent>
          <ItemTitle>
            <h2 id="running-here-heading">Running here</h2>
          </ItemTitle>
        </ItemContent>
        {count === 0 ? (
          <Item variant="outline">
            <ItemContent>
              <ItemDescription>{runsHere(server)}</ItemDescription>
            </ItemContent>
          </Item>
        ) : (
          <Item variant="outline" render={<Link from={Route.fullPath} to="." search={{ services: true }} />}>
            <ItemContent>
              <ItemTitle>{count === 1 ? "1 service" : `${count} services`}</ItemTitle>
              {onlyHere > 0 ? (
                <ItemDescription>
                  <ServerStatusLabel status="offline" stale={stale}>{onlyHere} run only here</ServerStatusLabel>
                </ItemDescription>
              ) : null}
            </ItemContent>
            <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
          </Item>
        )}
        <StrayNamespaces organizationSlug={organizationSlug} server={server} servers={servers} />
      </ItemGroup>
      <Sheet open={open && count > 0} onOpenChange={(next) => { if (!next) void navigate({ to: ".", search: {} }); }}>
        <SheetContent className="gap-0">
          <SheetHeader>
            <SheetTitle>Running on {server.name}</SheetTitle>
          </SheetHeader>
          <ServiceList server={server} up={up} stale={stale} />
        </SheetContent>
      </Sheet>
    </section>
  );
}

/** The Services with a container on this Server. While it is offline, each says whether it still runs elsewhere. */
function ServiceList({ server, up, stale }: {
  server: Server;
  up: readonly Server[];
  stale: boolean;
}) {
  const [filter, setFilter] = useState("");
  const rows = server.services.map((service) => ({ service, where: service.namespace }));
  const needle = filter.trim().toLowerCase();
  const shown = needle
    ? rows.filter(({ service, where }) => `${service.name} ${where ?? ""}`.toLowerCase().includes(needle))
    : rows;

  return (
    <>
      <div className="px-4 pb-3">
        <Input
          type="search"
          aria-label="Filter services"
          placeholder="Filter services"
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
        />
      </div>
      <ItemGroup className="min-h-0 flex-1 overflow-y-auto px-4 pb-4">
        {shown.length === 0 ? (
          <ItemDescription>No services match “{filter.trim()}”</ItemDescription>
        ) : shown.map(({ service, where }) => {
          const elsewhere = up.find((other) => service.machineIds.has(other.machine.id));
          return (
            <Item
              key={service.identity}
              variant="outline"
              size="sm"
            >
              <ItemMedia variant="icon"><BoxIcon /></ItemMedia>
              <ItemContent className="min-w-0">
                <ItemTitle>{service.name}</ItemTitle>
                {where ? <ItemDescription className="line-clamp-1">{where}</ItemDescription> : null}
              </ItemContent>
              <ItemActions>
                {server.status !== "offline" ? null : elsewhere ? (
                  <ItemDescription>Still on {elsewhere.name}</ItemDescription>
                ) : (
                  <ServerStatusLabel status="offline" stale={stale}>Down</ServerStatusLabel>
                )}
              </ItemActions>
            </Item>
          );
        })}
      </ItemGroup>
    </>
  );
}
