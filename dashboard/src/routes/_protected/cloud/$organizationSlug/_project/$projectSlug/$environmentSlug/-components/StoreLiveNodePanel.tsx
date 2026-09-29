import { Link, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { ArrowUpRightIcon, TriangleAlertIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { FieldDescription, FieldGroup, FieldLegend, FieldSet } from "#/components/ui/field";
import { Item, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { cn } from "#/lib/utils";
import { liveNodes } from "#/modules/config-store/store-branches";
import { branchQuery, environmentSettingsQuery, requireView, servicesQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { CanvasInspectorError } from "./CanvasInspectorRouteStates";
import { storeLiveLabel } from "./canvas/StoreLiveNode";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "./environment-route-paths";

/**
 * A Live Node's panel over the Config Store, by its name: whose it is, which Services here read it, a way to open where
 * it runs, and Make it separate, which stages an Own Copy here (Copy).
 */
export function StoreLiveNodePanel({ name }: { name: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const writer = useStoreWriter(params.organizationSlug);
  const navigate = useNavigate();
  const branch = useStoreView(params.organizationSlug, branchQuery(store));
  const settings = requireView(useStoreView(params.organizationSlug, environmentSettingsQuery(store)));
  const services = requireView(useStoreView(params.organizationSlug, servicesQuery(store))).services;
  const live = branch.ok ? liveNodes(branch.value.live, settings, services).find((node) => node.name === name) : undefined;
  if (!live) return <CanvasInspectorError noun="Live service" />;

  // Once staged it's no longer live here; the canvas shows it as a new Service of this Branch.
  function makeSeparate() {
    void writer.commit({ command: "copy_node", environment: store, node: name, expect: null });
    void navigate({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, search: {} });
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">{name}</span>
        <p className="truncate text-sm text-muted-foreground">{storeLiveLabel(live)}</p>
      </CanvasInspectorHeader>
      <FieldGroup className="min-h-0 flex-1 overflow-y-auto p-4">
        <FieldSet>
          <FieldLegend>Whose it is</FieldLegend>
          <FieldDescription>{live.owner ? `Changing it changes ${live.owner}.` : "Services here can't reach it."}</FieldDescription>
          {live.owner && live.data && (
            <Item variant="outline" state="warning" size="sm" role="note">
              <ItemMedia><TriangleAlertIcon className="text-warning" /></ItemMedia>
              <ItemContent>
                <ItemDescription className="text-foreground">This branch writes to {live.owner}'s real data.</ItemDescription>
              </ItemContent>
            </Item>
          )}
          {live.owner && (
            <Link to={ENVIRONMENT_INDEX_ROUTE_TO} params={{ ...params, environmentSlug: live.owner }}
              className={cn(buttonVariants({ variant: "outline" }), "self-start")}>
              Open in {live.owner}
              <ArrowUpRightIcon data-icon="inline-end" />
            </Link>
          )}
        </FieldSet>
        {live.owner && (
          <FieldSet>
            <FieldLegend>Separate</FieldLegend>
            {live.data ? <FieldDescription>Starts with empty data.</FieldDescription> : null}
            <Button variant="outline" className="self-start" onClick={makeSeparate}>
              {live.data ? "Give it a new, empty one" : "Make it separate"}
            </Button>
          </FieldSet>
        )}
        <FieldSet>
          <FieldLegend>Used here by</FieldLegend>
          <ItemGroup className="gap-1">
            {services.filter((service) => live.usedBy.includes(service.id)).map((service) => (
              <Item key={service.id} size="xs" variant="outline"
                render={<Link to={ENVIRONMENT_SERVICE_ROUTE_TO} params={{ ...params, serviceId: service.id }} search={{}} />}>
                <ItemContent><ItemTitle>{service.name}</ItemTitle></ItemContent>
              </Item>
            ))}
          </ItemGroup>
        </FieldSet>
      </FieldGroup>
    </div>
  );
}
