import { refusalFor } from "#/modules/organization/permissions";
import { regionLabels } from "#/modules/machines/server-regions";
import { useRegions } from "#/modules/machines/server-regions.hooks";
import { needsAttention } from "#/modules/machines/server-status";
import type { Server } from "#/modules/machines/use-servers";
import { useStrayNamespaces } from "#/modules/machines/namespace-cleanup.queries";
import { useRoleGate } from "#/modules/organization/members.queries";
import { drainDialogRows, drainLeftRows, removeHint } from "#/modules/machines/server-drain-view";
import { useServerDrain } from "#/modules/machines/server-drain.hooks";
import { OPERATE_REFUSED } from "#/modules/machines/server-policy.hooks";
import { useServerWebTraffic } from "#/modules/machines/server-web-traffic.hooks";
import { DrainDialog } from "./drain-dialog";
import { RemoveServerHint, RemoveServerSection } from "./remove-server-section";
import { ServerBuildsSection } from "./server-builds-section";
import { needsServerOnline } from "./server-page";
import { ServerRegionSection } from "./server-region-section";
import { DrainButton, ServerServicesSection } from "./server-services-section";
import { ServerWebTrafficSection } from "./server-web-traffic-section";
import { WebTrafficDialog } from "./web-traffic-dialog";

const REMOVE_REFUSED = refusalFor("servers:remove", "remove servers");

/**
 * Services, Web traffic, Builds and Danger: the sections that share one Drain, whose dialog opens from Services and
 * from Danger, and the Web traffic switch with its own confirm.
 */
export function ServerSettings({ organizationSlug, server, servers, stale }: {
  organizationSlug: string;
  server: Server;
  servers: readonly Server[];
  stale: boolean;
}) {
  const { machine } = server;
  const drain = useServerDrain(organizationSlug, { id: machine.id, name: server.name }, servers);
  const webTraffic = useServerWebTraffic(organizationSlug, { id: machine.id, name: server.name }, machine, servers);
  const operate = useRoleGate(organizationSlug, "servers:operate", OPERATE_REFUSED);
  const remove = useRoleGate(organizationSlug, "servers:remove", REMOVE_REFUSED);
  const strays = useStrayNamespaces(organizationSlug, server.services.flatMap((service) => service.namespace ?? []));
  const machines = servers.map((each) => each.machine);
  const regions = useRegions(organizationSlug, regionLabels(machines));
  // A Drain cordons this Server and copies images from it: it needs the Server online, and the page to see it.
  const unavailable = needsServerOnline("Drain", server, stale);
  const webUnavailable = needsServerOnline("Web traffic", server, stale);
  return (
    <>
      <ServerServicesSection
        machine={machine}
        organizationSlug={organizationSlug}
        lastToRunServices={!servers.some((other) => other.machine.id !== machine.id && other.machine.acceptsServices && !needsAttention(other.status))}
        view={drain.view}
        left={drainLeftRows(server.services, strays)}
        onDrain={drain.ask}
        unavailable={unavailable}
        gate={operate}
      />
      <ServerWebTrafficSection view={webTraffic.view} onTurnOn={webTraffic.turnOn} onAskOff={webTraffic.ask} unavailable={webUnavailable} gate={operate} />
      <ServerBuildsSection machine={machine} organizationSlug={organizationSlug} gate={operate} />
      <ServerRegionSection machine={machine} machines={machines} services={server.services} regions={regions} organizationSlug={organizationSlug} gate={operate} />
      <RemoveServerSection
        machine={machine}
        organizationSlug={organizationSlug}
        last={servers.length === 1}
        offline={server.status === "offline"}
        gate={remove}
        hint={(
          <RemoveServerHint
            hint={removeHint(drain.view, server.services, strays)}
            drainButton={<DrainButton view={drain.view} onClick={drain.ask} size="sm" disabled={unavailable !== null || !operate.allowed} title={operate.reason} />}
          />
        )}
      />
      <DrainDialog serverName={server.name} rows={drainDialogRows(drain.view, server.machine.id, server.services, strays)} {...drain.dialog} />
      <WebTrafficDialog serverName={server.name} domains={webTraffic.domains} partial={webTraffic.partial} reach={webTraffic.reach} {...webTraffic.dialog} />
    </>
  );
}
