import {
  Suspense, useEffect, useRef, useState,
  type ReactNode,
} from "react";
import { useHydrated, useNavigate, useParams } from "@tanstack/react-router";
import { cn } from "#/lib/utils";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "./environment-route-paths";
import { CanvasInspectorPending } from "./CanvasInspectorRouteStates";
import { InspectorPresentation } from "./CanvasInspectorHeader";

export function CanvasInspectorOverlay({
  children,
  canvas,
  selection,
}: {
  children: ReactNode;
  canvas: ReactNode;
  /**
   * `lit`: the panel lights up the canvas (a Deployment Page), so the shade stays clear.
   * `returnTo`: the service whose panel opened this one; closing goes back to its Deployments tab.
   */
  selection: { key: string; nodeId: string; lit?: boolean; returnTo?: string | null } | null;
}) {
  const navigate = useNavigate();
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const workspaceRef = useRef<HTMLDivElement>(null);
  const inspectorRef = useRef<HTMLElement>(null);
  const previousNode = useRef<string | null>(null);
  const selectionKey = selection?.key ?? null;
  const [preference, setPreference] = useState({ key: selectionKey, full: false });
  const isHydrated = useHydrated();
  const takeover = preference.key === selectionKey && preference.full;

  if (preference.key !== selectionKey) {
    setPreference({ key: selectionKey, full: false });
  }

  useEffect(() => {
    const workspace = workspaceRef.current;
    if (selectionKey) {
      previousNode.current = selection?.nodeId ?? null;
      const inspector = inspectorRef.current;
      const exit = [...(inspector?.querySelectorAll<HTMLElement>("[data-canvas-inspector-exit]") ?? [])]
        .find((element) => element.getBoundingClientRect().width > 0);
      (exit ?? inspector)?.focus({ preventScroll: true });
    } else if (previousNode.current && workspace) {
      const nodeId = previousNode.current;
      // Both canvas links and the mobile list expose the same stable node identity.
      const node = [...workspace.querySelectorAll<HTMLElement>("[data-canvas-node]")]
        .find((element) => element.dataset["canvasNode"] === nodeId && element.getBoundingClientRect().width > 0);
      (node ?? workspace).focus({ preventScroll: true });
      previousNode.current = null;
    }
  }, [selectionKey, selection?.nodeId]);

  const returnTo = selection?.returnTo ?? null;
  function closeInspector() {
    const viewTransition = { types: ["canvas-inspector-close"] };
    void (returnTo
      ? navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: returnTo }, search: { tab: "deployments" }, viewTransition })
      : navigate({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, search: (previous) => ({ ...previous, tab: undefined }), viewTransition }));
  }

  return (
    <div
      ref={workspaceRef}
      role="region"
      aria-label="Canvas"
      tabIndex={-1}
      className="environment-canvas-scene"
    >
      {canvas}
      {selection ? <>
        <button
          className="canvas-inspector-shade"
          data-clear={selection.lit || undefined}
          type="button"
          tabIndex={-1}
          aria-label="Close inspector and return to Canvas"
          onClick={closeInspector}
        />
        <section
          ref={inspectorRef}
          aria-label="Resource inspector"
          tabIndex={-1}
          data-canvas-inspector-pane
          data-takeover={takeover}
          className={cn("canvas-inspector-pane", isHydrated && "canvas-inspector-enter")}
          onKeyDown={(event) => {
            if (event.key !== "Escape" || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
            if (!(event.target instanceof Node) || !event.currentTarget.contains(event.target)) return;
            event.preventDefault();
            event.stopPropagation();
            closeInspector();
          }}
        >
          <InspectorPresentation value={{
            takeover,
            returnTo,
            toggleFullscreen: () => setPreference({ key: selectionKey, full: !preference.full }),
          }}>
            <Suspense fallback={<CanvasInspectorPending />}>{children}</Suspense>
          </InspectorPresentation>
        </section>
      </> : null}
    </div>
  );
}
