import { Link, useParams } from "@tanstack/react-router";
import { plural } from "#/lib/plural";
import { ArrowUpIcon, LayersIcon, Maximize2Icon } from "lucide-react";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "#/components/ui/card";
import { cn } from "#/lib/utils";
import type { InstallView } from "#/modules/config-store/templates";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_TEMPLATE_ROUTE_TO } from "../environment-route-paths";
import { useCanvasInspectorSelection } from "../useCanvasInspectorSelection";
import { templateNodeId } from "./nodes";
import { useTemplateView } from "./template-view";
import type { CanvasStoreTemplateNode } from "./types";

/** A template as one node. Opens its settings; the corner button expands its Services. */
export function StoreTemplateNode({ data: { instance } }: { data: CanvasStoreTemplateNode["data"] }) {
  return <StoreTemplateCard instance={instance} className="h-36 w-72" />;
}

export function StoreTemplateCard({ instance, className }: { instance: InstallView; className: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { selectedNodeId } = useCanvasInspectorSelection();
  const { setOpen } = useTemplateView();
  const { template, install, release, latest, members, toFix } = instance;
  const id = templateNodeId(install.name);
  return (
    <div className={cn("relative", className)}>
      {/* Stacked edges: more than one Service inside. */}
      <div aria-hidden className="absolute inset-x-4 -bottom-3 h-full rounded-xl border bg-card" />
      <div aria-hidden className="absolute inset-x-2 -bottom-1.5 h-full rounded-xl border bg-card" />
      <Link to={ENVIRONMENT_TEMPLATE_ROUTE_TO} params={{ ...params, templateId: install.name }} search={{}}
        data-canvas-node={id} draggable={false} className="relative z-10 block size-full rounded-xl">
        <Card size="node" className="h-full justify-between" data-selected={selectedNodeId === id}>
          <CardHeader>
            <div className="flex items-start gap-3">
              <Avatar><AvatarFallback><LayersIcon className="size-4" /></AvatarFallback></Avatar>
              <CardTitle className="min-w-0 flex-1 truncate">{install.name}</CardTitle>
              {toFix > 0 && <Badge variant="warning">{toFix} to fix</Badge>}
              {latest && toFix === 0 && <Badge variant="changed"><ArrowUpIcon data-icon="inline-start" />v{latest.version}</Badge>}
            </div>
          </CardHeader>
          <CardContent className="pr-12 text-sm text-muted-foreground">{template.name} v{release.version} · {plural(members.length, "service")}</CardContent>
        </Card>
      </Link>
      <Button type="button" variant="ghost" size="icon-sm" aria-label={`Expand ${install.name}`} title="Expand"
        className="absolute right-3 bottom-3 z-20" onClick={() => setOpen(install.name)}>
        <Maximize2Icon />
      </Button>
    </div>
  );
}
