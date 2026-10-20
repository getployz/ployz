import { createFileRoute, Link } from "@tanstack/react-router";
import { Schema } from "effect";
import { DashboardPage } from "#/components/dashboard-page";
import { Button } from "#/components/ui/button";
import { useServers } from "#/modules/machines/use-servers";
import { serverEnrollmentsQueryOptions, waitingServersQueryOptions } from "#/modules/machines/waiting/waiting.queries";
import { NEWEST_RELEASE } from "#/modules/server-upgrade/server-upgrade";
import { channelReleaseQueryOptions, latestServerUpgradesQueryOptions } from "#/modules/server-upgrade/server-upgrade.queries";
import { prefetchRemote } from "#/collections/route-data";
import { AddServersDialog } from "./-components/add-servers-dialog";
import { enrollingRows } from "./-components/enrolling-list";
import { FirstRunCard } from "./-components/first-run-card";
import { FleetHealth } from "./-components/fleet-health";
import { SERVER_LIST, ServerRow } from "./-components/server-row";
import { serversPageState } from "./-components/servers-page-state";
import { ServersSkeleton } from "./-components/servers-skeleton";
import { ServersStaleAlert, ServersUnreachable } from "./-components/servers-unreachable";
import { ServersUpgradeBar, useServersUpgrade } from "./-components/servers-upgrade";
import { useEnrollmentStatus, useWaitingServers } from "./-components/use-waiting";
import { IntakeTray } from "./-components/waiting-list";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ token: Schema.optional(Schema.String) })),
  // The release on the Servers' own line waits for the versions they report.
  loader: ({ params, context }) => prefetchRemote(
    context,
    latestServerUpgradesQueryOptions(params.organizationSlug),
    channelReleaseQueryOptions(NEWEST_RELEASE),
    waitingServersQueryOptions(params.organizationSlug),
    serverEnrollmentsQueryOptions(params.organizationSlug),
  ),
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { token } = Route.useSearch();
  const { state, servers, unreachable } = useServers(organizationSlug);
  const upgrades = useServersUpgrade(organizationSlug, servers);
  const waiting = useWaitingServers(organizationSlug);
  const enrollments = useEnrollmentStatus(organizationSlug);
  const page = serversPageState({
    list: state,
    servers: servers.length,
    waiting: waiting.data?.length,
    enrolling: enrollments.data === undefined ? undefined : enrollingRows(enrollments.data.attempts).length,
  });
  const upgrade = page.kind === "fleet" && !page.stale ? upgrades : null;
  // First run has its own install card; the dialog returns once a server has shown up.
  const canAdd = page.kind !== "first-run" || page.intake;

  return (
    <DashboardPage width="content">
      {/* The top bar names the page; each row carries its own status, so this bar holds only actions. */}
      <div className="flex items-center justify-end gap-2">
        {upgrade === null ? null : <ServersUpgradeBar organizationSlug={organizationSlug} upgrade={upgrade} />}
        <Button variant="ghost" nativeButton={false} render={<Link to="/cloud/$organizationSlug/~/servers/tokens" params={{ organizationSlug }} />}>
          Tokens
        </Button>
        {canAdd ? (
          <AddServersDialog
            organizationSlug={organizationSlug}
            label={upgrade === null ? "Add servers" : <span className="max-sm:sr-only">Add servers</span>}
          />
        ) : null}
      </div>
      {waiting.isError || enrollments.isError ? (
        <p role="alert">
          Couldn’t load waiting servers or enrollments.{" "}
          <Button variant="outline" onClick={() => { void waiting.refetch(); void enrollments.refetch(); }}>Retry</Button>
        </p>
      ) : null}
      {/* Above the list: the skeleton below is a guess at the list's height, and the tray must not jump when it settles. */}
      {page.intake ? <IntakeTray organizationSlug={organizationSlug} token={token} firstRun={page.kind === "first-run"} serversOffline={unreachable} /> : null}
      {page.kind === "loading" ? (
        <ServersSkeleton />
      ) : page.kind === "unreachable" ? (
        <ServersUnreachable organizationSlug={organizationSlug} />
      ) : page.kind === "first-run" ? (
        <FirstRunCard key={organizationSlug} organizationSlug={organizationSlug} arrived={page.intake} />
      ) : (
        <>
          {page.stale ? <ServersStaleAlert /> : null}
          <FleetHealth servers={servers} upgrades={upgrade?.rows ?? new Map()} />
          <nav aria-label="Servers" className={SERVER_LIST}>
            {servers.map((server) => (
              <ServerRow
                key={server.machine.id}
                organizationSlug={organizationSlug}
                server={server}
                upgrade={upgrade?.rows.get(server.machine.id) ?? null}
                stale={page.stale}
              />
            ))}
          </nav>
        </>
      )}
    </DashboardPage>
  );
}
