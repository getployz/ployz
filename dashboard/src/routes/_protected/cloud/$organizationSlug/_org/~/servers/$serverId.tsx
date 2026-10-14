import { Suspense, useState, type ReactNode } from "react";
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
import { latestServerUpgradesQueryOptions } from "#/modules/server-upgrade/server-upgrade.queries";
import { useStrayNamespaces } from "#/modules/machines/namespace-cleanup.queries";
import { drainDialogRows, drainLeftRows, removeHint } from "#/modules/machines/server-drain-view";
import { useServerDrain } from "#/modules/machines/server-drain.hooks";
import { latestServerDrainsQueryOptions } from "#/modules/machines/server-drain.queries";
import { useServerWebTraffic } from "#/modules/machines/server-web-traffic.hooks";
import { latestServerWebTrafficQueryOptions } from "#/modules/machines/server-web-traffic.queries";
import { volumePlacementsQueryOptions } from "#/modules/volume-placement/volume-placement.queries";
import { prefetchRemote } from "#/collections/route-data";
import { DrainDialog } from "./-components/drain-dialog";
import { RemoveServerHint, RemoveServerSection } from "./-components/remove-server-section";
import { runsHere } from "./-components/runs-here";
import { ServerBuildsSection } from "./-components/server-builds-section";
import { ServerResources } from "./-components/server-resources";
import { DrainButton, ServerServicesSection } from "./-components/server-services-section";
import { ServerSwitcher } from "./-components/server-switcher";
import { ServerUpgradeSection } from "./-components/server-upgrade-section";
import { ServerWebTrafficSection } from "./-components/server-web-traffic-section";
import { ServiceUsage } from "./-components/service-usage";
import { StrayNamespaces } from "./-components/stray-namespaces";
import { WebTrafficDialog } from "./-components/web-traffic-dialog";
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
  loader: ({ params, context }) => prefetchRemote(
    context,
    latestServerUpgradesQueryOptions(params.organizationSlug),
    latestServerDrainsQueryOptions(params.organizationSlug),
    latestServerWebTrafficQueryOptions(params.organizationSlug),
    volumePlacementsQueryOptions(params.organizationSlug),
  ),
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
      {server.status === "offline" ? null : <ServerResources organizationSlug={organizationSlug} server={{ id: machine.id, name: server.name }} />}
      <RunningHere organizationSlug={organizationSlug} server={server} servers={servers} stale={stale} />
      <Suspense fallback={null}>
        <ServerUpgradeSection machine={machine} status={server.status} organizationSlug={organizationSlug} />
      </Suspense>
      <Suspense fallback={null}>
        <ServerSettings organizationSlug={organizationSlug} server={server} servers={servers} stale={stale} />
      </Suspense>
    </DashboardPage>
  );
}

/**
 * Services, Web traffic, Builds and Danger: the sections that share one Drain, whose dialog opens from Services and
 * from Danger, and the Web traffic switch with its own confirm.
 */
function ServerSettings({ organizationSlug, server, servers, stale }: {
  organizationSlug: string;
  server: Server;
  servers: readonly Server[];
  stale: boolean;
}) {
  const { machine } = server;
  const drain = useServerDrain(organizationSlug, { id: machine.id, name: server.name }, servers);
  const webTraffic = useServerWebTraffic(organizationSlug, { id: machine.id, name: server.name }, machine, servers);
  const strays = useStrayNamespaces(organizationSlug, server.services.flatMap((service) => service.namespace ?? []));
  // A Drain cordons this Server and copies images from it: it needs the Server online, and the page to see it.
  const unavailable = stale
    ? "Drain needs Cloud to reach your servers."
    : server.status === "offline" ? `Drain needs ${server.name} online.` : null;
  const webUnavailable = stale
    ? "Web traffic needs Cloud to reach your servers."
    : server.status === "offline" ? `Web traffic needs ${server.name} online.` : null;
  return (
    <>
      <ServerServicesSection
        machine={machine}
        organizationSlug={organizationSlug}
        view={drain.view}
        left={drainLeftRows(server.services, strays)}
        onDrain={drain.ask}
        unavailable={unavailable}
      />
      <ServerWebTrafficSection view={webTraffic.view} onTurnOn={webTraffic.turnOn} onAskOff={webTraffic.ask} unavailable={webUnavailable} />
      <ServerBuildsSection machine={machine} organizationSlug={organizationSlug} />
      <RemoveServerSection
        machine={machine}
        organizationSlug={organizationSlug}
        last={servers.length === 1}
        offline={server.status === "offline"}
        hint={(
          <RemoveServerHint
            hint={removeHint(drain.view, server.services, strays)}
            drainButton={<DrainButton view={drain.view} onClick={drain.ask} size="sm" disabled={unavailable !== null} />}
          />
        )}
      />
      <DrainDialog serverName={server.name} rows={drainDialogRows(drain.view, server.machine.id, server.services, strays)} {...drain.dialog} />
      <WebTrafficDialog serverName={server.name} domains={webTraffic.domains} partial={webTraffic.partial} reach={webTraffic.reach} {...webTraffic.dialog} />
    </>
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

/** Rows shown in place; each reads its Service's usage, so the rest wait in a sheet that reads only while open. */
const SHOWN_HERE = 5;

/**
 * The Services on this Server with what each uses here now. While the Server is offline, each row says whether its
 * Service still runs elsewhere, and the sheet link counts those that run nowhere else.
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
  const row = (service: Server["services"][number]) => (
    <ServiceRow key={service.identity} organizationSlug={organizationSlug} server={server} service={service} up={up} stale={stale} />
  );

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
        ) : server.services.slice(0, SHOWN_HERE).map(row)}
        {count > SHOWN_HERE ? (
          <Item variant="outline" size="sm" render={<Link from={Route.fullPath} to="." search={{ services: true }} />}>
            <ItemContent>
              <ItemTitle>All {count} services</ItemTitle>
              {onlyHere > 0 ? (
                <ItemDescription>
                  <ServerStatusLabel status="offline" stale={stale}>{onlyHere} run only here</ServerStatusLabel>
                </ItemDescription>
              ) : null}
            </ItemContent>
            <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
          </Item>
        ) : null}
        <StrayNamespaces organizationSlug={organizationSlug} server={server} servers={servers} />
      </ItemGroup>
      <Sheet open={open && count > SHOWN_HERE} onOpenChange={(next) => { if (!next) void navigate({ to: ".", search: {} }); }}>
        <SheetContent className="gap-0">
          <SheetHeader>
            <SheetTitle>Running on {server.name}</SheetTitle>
          </SheetHeader>
          <ServiceList services={server.services} row={row} />
        </SheetContent>
      </Sheet>
    </section>
  );
}

function ServiceList({ services, row }: {
  services: Server["services"];
  row: (service: Server["services"][number]) => ReactNode;
}) {
  const [filter, setFilter] = useState("");
  const needle = filter.trim().toLowerCase();
  const shown = needle
    ? services.filter((service) => `${service.name} ${service.namespace ?? ""}`.toLowerCase().includes(needle))
    : services;

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
        {shown.length === 0 ? <ItemDescription>No services match “{filter.trim()}”</ItemDescription> : shown.map(row)}
      </ItemGroup>
    </>
  );
}

function ServiceRow({ organizationSlug, server, service, up, stale }: {
  organizationSlug: string;
  server: Server;
  service: Server["services"][number];
  up: readonly Server[];
  stale: boolean;
}) {
  const elsewhere = up.find((other) => service.machineIds.has(other.machine.id));
  return (
    <Item variant="outline" size="sm">
      <ItemMedia variant="icon"><BoxIcon /></ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle>{service.name}</ItemTitle>
        {service.namespace ? <ItemDescription className="line-clamp-1">{service.namespace}</ItemDescription> : null}
      </ItemContent>
      <ItemActions>
        {server.status !== "offline" ? (
          service.namespace ? (
            <ServiceUsage
              organizationSlug={organizationSlug}
              server={{ id: server.machine.id, name: server.name }}
              namespace={service.namespace}
              name={service.name}
            />
          ) : null
        ) : elsewhere ? (
          <ItemDescription>Still on {elsewhere.name}</ItemDescription>
        ) : (
          <ServerStatusLabel status="offline" stale={stale}>Down</ServerStatusLabel>
        )}
      </ItemActions>
    </Item>
  );
}
