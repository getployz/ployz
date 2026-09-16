import { Handle, Position } from "@xyflow/react";
import { Link, useParams } from "@tanstack/react-router";
import { DatabaseIcon } from "lucide-react";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Badge } from "#/components/ui/badge";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import { useCanvasEnvironmentResource } from "./CanvasServicesContext";
import {
  ENVIRONMENT_RESOURCE_ROUTE_TO,
  ENVIRONMENT_ROUTE_FROM,
} from "../environment-route-paths";
import type { CanvasVariableGroupNodeData } from "./types";

function VariableGroupLoadingNode() {
  return (
    <Card size="node" className="h-36 w-72 justify-between">
      <CardHeader>
        <div className="flex items-start gap-3">
          <Skeleton className="size-8" />
          <div className="min-w-0 flex-1">
            <Skeleton className="h-5 w-28" />
            <Skeleton className="mt-1.5 h-4 w-20" />
          </div>
          <Skeleton className="h-5 w-16" />
        </div>
      </CardHeader>
      <CardContent>
        <Skeleton className="h-4 w-40" />
      </CardContent>
    </Card>
  );
}

export function VariableGroupNode({
  data,
  selected,
}: {
  data: CanvasVariableGroupNodeData;
  selected?: boolean;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const resourceState = useCanvasEnvironmentResource(data.resourceId);

  if (!resourceState) {
    return <VariableGroupLoadingNode />;
  }

  const resource = resourceState.resource;
  const exportKeys = resource.exports.map((item) => item.key);
  const exportSummary =
    exportKeys.length === 0
      ? "No exports"
      : exportKeys.slice(0, 2).join(", ");
  const overflowCount = Math.max(exportKeys.length - 2, 0);

  return (
    <Link
      to={ENVIRONMENT_RESOURCE_ROUTE_TO}
      params={{
        organizationSlug: params.organizationSlug,
        projectSlug: params.projectSlug,
        environmentSlug: params.environmentSlug,
        resourceId: data.resourceId,
      }}
      search={(prev) => prev}
      preload="intent"
      draggable={false}
      className="block h-36 w-72"
    >
      <Handle
        type="target"
        position={Position.Bottom}
        isConnectable={false}
        className="opacity-0"
      />
      <Handle
        type="source"
        position={Position.Top}
        isConnectable={false}
        className="opacity-0"
      />
      <Card
        size="node"
        className="h-full justify-between"
        data-selected={selected}
      >
        <CardHeader>
          <div className="flex items-start gap-3">
            <Avatar>
              <AvatarFallback>
                <DatabaseIcon />
              </AvatarFallback>
            </Avatar>
            <div className="min-w-0 flex-1 overflow-hidden">
              <CardTitle>
                {resource.resource.name}
              </CardTitle>
              <CardDescription>
                Variable Group
              </CardDescription>
            </div>
            <Badge variant="secondary">
              {resource.consumerCount}
            </Badge>
            {resourceState.diffRowCount > 0 ? (
              <Badge variant="changed">
                {resourceState.diffRowCount}
              </Badge>
            ) : null}
          </div>
        </CardHeader>
        <CardContent>
          <p className="truncate text-muted-foreground">
            {overflowCount > 0
              ? `${exportSummary}, +${overflowCount}`
              : exportSummary}
          </p>
        </CardContent>
      </Card>
    </Link>
  );
}
