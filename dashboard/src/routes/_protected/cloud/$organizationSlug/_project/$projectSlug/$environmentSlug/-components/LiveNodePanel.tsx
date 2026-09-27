import { useLiveQuery } from "@tanstack/react-db";
import { Link, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { ArrowUpRightIcon, TriangleAlertIcon } from "lucide-react";
import { getRawServicesCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { cn } from "#/lib/utils";
import { FieldDescription, FieldGroup, FieldLegend, FieldSet } from "#/components/ui/field";
import { Item, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { useLiveNodes } from "#/modules/branches/use-live-nodes";
import { useBranchUnsettled } from "#/modules/branches/branch.collection";
import { useUpdateBranch } from "#/modules/branches/branch-commands";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { CanvasInspectorError } from "./CanvasInspectorRouteStates";
import { nodeDestination } from "./environment-node-navigation";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "./environment-route-paths";
import { liveNodeLabel } from "./canvas/LiveServiceNode";

/**
 * A Live Node's panel: whose it is, which services here use it, a way to open it in its own Environment, and Make it an
 * Own Copy, which stages it here from its owner under the same gate as Update.
 */
export function LiveNodePanel({ lineageId }: { lineageId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const liveNode = useLiveNodes(params.organizationSlug, environmentId).find((node) => node.lineageId === lineageId);
  const { data: services } = useLiveQuery(getRawServicesCollection(params.organizationSlug, useCollectionScope()));
  const { makeOwnCopy: ownCopy } = useUpdateBranch(params.organizationSlug);
  const unsettled = useBranchUnsettled(params.organizationSlug, environmentId);
  const navigate = useNavigate();
  if (!liveNode) return <CanvasInspectorError noun="Live service" />;
  const owner = liveNode.owner;

  // Once staged it's no longer live here; the canvas shows it as a new node of this branch.
  function makeOwnCopy() {
    ownCopy(environmentId, lineageId);
    void navigate({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, search: {} });
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">{liveNode.name}</span>
        <p className="truncate text-sm text-muted-foreground">{liveNodeLabel(liveNode)}</p>
      </CanvasInspectorHeader>
      <FieldGroup className="min-h-0 flex-1 overflow-y-auto p-4">
        <FieldSet>
          <FieldLegend>Whose it is</FieldLegend>
          <FieldDescription>
            {owner
              ? `${liveNode.name} is ${owner.environment.name}'s. This branch uses it live instead of its own copy, so changing it changes ${owner.environment.name}.`
              : `No environment this branch comes from runs ${liveNode.name}, so what uses it here can't reach it.`}
          </FieldDescription>
          {owner?.ownsData && (
            <Item variant="outline" state="warning" size="sm" role="note">
              <ItemMedia><TriangleAlertIcon className="text-warning" /></ItemMedia>
              <ItemContent>
                <ItemDescription className="text-foreground">
                  It keeps real data: this branch reads and writes {owner.environment.name}'s.
                </ItemDescription>
              </ItemContent>
            </Item>
          )}
          {owner && (
            <Link
              {...nodeDestination({ ...params, environmentSlug: owner.environment.namespace }, { id: owner.node.nodeId, name: liveNode.name, type: "service" })}
              className={cn(buttonVariants({ variant: "outline" }), "self-start")}
            >
              Open in {owner.environment.name}
              <ArrowUpRightIcon data-icon="inline-end" />
            </Link>
          )}
        </FieldSet>
        {owner && (
          <FieldSet>
            <FieldLegend>Own copy</FieldLegend>
            <FieldDescription>
              {unsettled ?? `This branch would run its own ${liveNode.name}, made from ${owner.environment.name}'s${owner.ownsData ? ", with an empty copy of its data" : ""}. It deploys with the branch's next deploy.`}
            </FieldDescription>
            <Button variant="outline" className="self-start" onClick={makeOwnCopy} disabled={unsettled !== null}>
              Make it an Own Copy
            </Button>
          </FieldSet>
        )}
        <FieldSet>
          <FieldLegend>Used here by</FieldLegend>
          <ItemGroup className="gap-1">
            {liveNode.usedBy.map((serviceId) => (
              <Item key={serviceId} size="xs" variant="outline"
                render={<Link to={ENVIRONMENT_SERVICE_ROUTE_TO} params={{ ...params, serviceId }} search={{}} />}>
                <ItemContent><ItemTitle>{services.find((row) => row.id === serviceId)?.name ?? "A service"}</ItemTitle></ItemContent>
              </Item>
            ))}
          </ItemGroup>
        </FieldSet>
      </FieldGroup>
    </div>
  );
}
