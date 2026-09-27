import { cn } from "#/lib/utils";
import { createContext, useContext, type ComponentProps, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { ArrowLeftIcon, Maximize2Icon, Minimize2Icon, XIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_SERVICE_ROUTE_TO } from "./environment-route-paths";

// Shared by the frame and header; route selection remains owned by the router.
export const InspectorPresentation = createContext<{
  takeover: boolean;
  toggleFullscreen: () => void;
  /** The service whose panel opened this one (its Deployments tab); leaving goes back there instead of the canvas. */
  returnTo: string | null;
} | null>(null);

type CanvasInspectorHeaderParams = {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
};

export function CanvasInspectorHeader({ params, children }: {
  params: CanvasInspectorHeaderParams;
  children: ReactNode;
}) {
  const presentation = useContext(InspectorPresentation);
  if (!presentation) throw new Error("Canvas inspector header must be inside its workspace");
  const { takeover, toggleFullscreen, returnTo } = presentation;
  const back = returnTo ? "Back to service" : "Back to Canvas";
  const returnLink = (
    <ExitLink params={params} returnTo={returnTo}
      className={cn(buttonVariants({ variant: "ghost", size: "icon" }), "canvas-inspector-back")}
      data-canvas-inspector-exit aria-label={back} title={back}>
      <ArrowLeftIcon />
    </ExitLink>
  );

  return (
    <div className="canvas-inspector-header flex shrink-0 items-center gap-3 border-b px-4">
      {returnLink}
      <div className="min-w-0">{children}</div>
      <div className="ml-auto flex shrink-0 items-center gap-3">
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
          <ExitLink params={params} returnTo={returnTo}
            className={cn(buttonVariants({ variant: "ghost", size: "icon" }), "canvas-inspector-close")}
            data-canvas-inspector-exit data-canvas-inspector-desktop-control aria-label="Close inspector" title="Close inspector">
            <XIcon />
          </ExitLink>
        ) : null}
      </div>
    </div>
  );
}

/** Leaves the panel: back to the service panel that opened it, else to the canvas. */
function ExitLink({ params, returnTo, ...props }: { params: CanvasInspectorHeaderParams; returnTo: string | null } & ComponentProps<"a">) {
  const transition = { types: ["canvas-inspector-close"] };
  return returnTo
    ? <Link {...props} to={ENVIRONMENT_SERVICE_ROUTE_TO} params={{ ...params, serviceId: returnTo }} search={{ tab: "deployments" }} viewTransition={transition} />
    : <Link {...props} to={ENVIRONMENT_INDEX_ROUTE_TO} params={params} search={(previous) => ({ ...previous, tab: undefined })} viewTransition={transition} />;
}
