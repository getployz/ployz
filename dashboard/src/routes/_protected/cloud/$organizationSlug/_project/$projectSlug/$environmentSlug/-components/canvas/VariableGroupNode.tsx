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
import { cn } from "#/lib/utils";
import { useCanvasEnvironmentResource } from "./CanvasServicesContext";
import {
  ENVIRONMENT_RESOURCE_ROUTE_TO,
  ENVIRONMENT_ROUTE_FROM,
} from "../environment-route-paths";
import type { CanvasVariableGroupNodeData } from "./types";

function VariableGroupLoadingNode() {
  return (
    <Card size="node" className="h-[144px] w-[288px] justify-between">
      <CardHeader>
        <div className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-start gap-3">
          <Skeleton className="size-8 rounded-full" />
          <div className="min-w-0">
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
      className="block h-[144px] w-[288px]"
    >
      <Handle
        type="target"
        position={Position.Bottom}
        isConnectable={false}
        style={{ opacity: 0 }}
      />
      <Handle
        type="source"
        position={Position.Top}
        isConnectable={false}
        style={{ opacity: 0 }}
      />
      <Card
        size="node"
        className={cn("h-full justify-between", selected && "ring-2 ring-ring")}
      >
        <CardHeader>
          <div className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-start gap-3">
            <Avatar>
              <AvatarFallback>
                <DatabaseIcon />
              </AvatarFallback>
            </Avatar>
            <div className="min-w-0 overflow-hidden">
              <CardTitle className="truncate">
                {resource.resource.name}
              </CardTitle>
              <CardDescription className="truncate">
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
