import { cn } from "#/lib/utils";
import { createContext, useContext, type ReactNode } from "react";
import { Link, linkOptions } from "@tanstack/react-router";
import { ArrowLeftIcon, Maximize2Icon, Minimize2Icon, XIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { ENVIRONMENT_INDEX_ROUTE_TO } from "./environment-route-paths";

export const InspectorPresentation = createContext<{
  takeover: boolean;
  toggleFullscreen: () => void;
} | null>(null);

type CanvasInspectorHeaderParams = {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
};

export function CanvasInspectorHeader({ params, actions, children }: {
  params: CanvasInspectorHeaderParams;
  /** The panel's own controls, before resize and close: a ⋮ menu. */
  actions?: ReactNode;
  children: ReactNode;
}) {
  const presentation = useContext(InspectorPresentation);
  if (!presentation) throw new Error("Canvas inspector header must be inside its workspace");
  const { takeover, toggleFullscreen } = presentation;
  const back = "Back to Architecture";
  const returnLink = (
    <Link {...inspectorExit(params)}
      className={cn(buttonVariants({ variant: "ghost", size: "icon" }), "canvas-inspector-back")}
      data-canvas-inspector-exit aria-label={back} title={back}>
      <ArrowLeftIcon />
    </Link>
  );

  return (
    <div className="canvas-inspector-header flex shrink-0 items-center gap-3 border-b px-4">
      {returnLink}
      <div className="min-w-0">{children}</div>
      <div className="ml-auto flex shrink-0 items-center gap-3">
        {actions}
        <Button
          variant="ghost"
          size="icon"
          data-canvas-inspector-resize
          onClick={toggleFullscreen}
          aria-label={takeover ? "Restore inspector" : "Fill canvas"}
          title={takeover ? "Restore inspector" : "Fill canvas"}
        >
          {takeover ? <Minimize2Icon /> : <Maximize2Icon />}
        </Button>
        {!takeover ? (
          <Link {...inspectorExit(params)}
            className={cn(buttonVariants({ variant: "ghost", size: "icon" }), "canvas-inspector-close")}
            data-canvas-inspector-exit data-canvas-inspector-desktop-control aria-label="Close inspector" title="Close inspector">
            <XIcon />
          </Link>
        ) : null}
      </div>
    </div>
  );
}

/** Where leaving a panel goes: the bare canvas. */
export function inspectorExit(params: CanvasInspectorHeaderParams) {
  return linkOptions({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, search: {}, viewTransition: { types: ["canvas-inspector-close"] } });
}
