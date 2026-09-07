import { Suspense, useRef, useState, type ReactNode } from "react";
import { useHydrated, useNavigate, useParams } from "@tanstack/react-router";
import {
  Drawer,
  DrawerContent,
  DrawerTitle,
} from "#/components/ui/drawer";
import { useIsMobile } from "#/hooks/use-mobile";
import { cn } from "#/lib/utils";
import {
  ENVIRONMENT_INDEX_ROUTE_TO,
  ENVIRONMENT_ROUTE_FROM,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";
import { CanvasInspectorPending } from "./CanvasInspectorRouteStates";

export function CanvasInspectorOverlay({
  children,
}: {
  children: ReactNode;
}) {
  const isMobile = useIsMobile();
  const navigate = useNavigate();
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const [drawerOpen, setDrawerOpen] = useState(true);
  const isHydrated = useHydrated();
  const enterAnimated = useRef(isHydrated).current;

  const content = (
    <Suspense fallback={<CanvasInspectorPending />}>{children}</Suspense>
  );

  if (isMobile) {
    return (
      <Drawer
        open={drawerOpen}
        showSwipeHandle
        onOpenChange={setDrawerOpen}
        onOpenChangeComplete={(open) => {
          if (open) {
            return;
          }

          void navigate({
            to: ENVIRONMENT_INDEX_ROUTE_TO,
            params,
            search: (prev) => prev,
            viewTransition: { types: ["canvas-inspector-close"] },
          });
        }}
      >
        <DrawerContent className="h-[calc(100dvh-6rem)]">
          <DrawerTitle className="sr-only">Canvas inspector</DrawerTitle>
          {content}
        </DrawerContent>
      </Drawer>
    );
  }

  return (
    <div className="pointer-events-none absolute top-0 right-2 bottom-2 left-2 z-10 mt-2 grid w-auto grid-cols-[minmax(0,1fr)] items-start justify-center sm:grid-cols-[0px_100%] lg:grid-cols-[1fr_70%] xl:grid-cols-[1fr_minmax(auto,_920px)] 2xl:grid-cols-[1fr_minmax(auto,_1024px)]">
      <div
        data-canvas-inspector-pane
        className={cn(
          "pointer-events-auto col-start-1 h-full min-w-0 max-w-full overflow-hidden rounded-xl border bg-background shadow-2xl [view-transition-name:canvas-inspector] sm:col-start-2",
          enterAnimated && "canvas-inspector-enter",
        )}
      >
        {content}
      </div>
    </div>
  );
}
