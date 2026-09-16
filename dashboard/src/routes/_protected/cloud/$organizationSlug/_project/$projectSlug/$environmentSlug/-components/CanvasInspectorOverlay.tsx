import { prefersReducedMotion } from "#/lib/motion";
import { Suspense, useRef, useState, type ReactNode, type RefObject } from "react";
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
  finalFocus,
}: {
  children: ReactNode;
  finalFocus: RefObject<HTMLDivElement | null>;
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
        onOpenChange={(open) => {
          setDrawerOpen(open);
          if (open) {
            return;
          }

          void navigate({
            to: ENVIRONMENT_INDEX_ROUTE_TO,
            params,
            search: (prev) => prev,
            viewTransition: prefersReducedMotion() ? false : { types: ["canvas-inspector-close"] },
          });
        }}
      >
        <DrawerContent
          className="h-[calc(100dvh-6rem)] [view-transition-name:canvas-drawer]"
          finalFocus={finalFocus}
        >
          <DrawerTitle className="sr-only">Canvas inspector</DrawerTitle>
          {content}
        </DrawerContent>
      </Drawer>
    );
  }

  return (
    <div className="pointer-events-none absolute top-0 right-2 bottom-2 left-2 z-10 mt-2 flex justify-end">
      <div
        data-canvas-inspector-pane
        className={cn(
          "pointer-events-auto h-full min-w-0 w-full lg:w-3/4 xl:max-w-5xl overflow-hidden rounded-xl border bg-background shadow-2xl [view-transition-name:canvas-inspector]",
          enterAnimated && "canvas-inspector-enter",
        )}
      >
        {content}
      </div>
    </div>
  );
}
