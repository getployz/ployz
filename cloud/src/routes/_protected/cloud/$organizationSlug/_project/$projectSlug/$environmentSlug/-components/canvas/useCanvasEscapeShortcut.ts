import { useEffect } from "react";
import { useNavigate } from "@tanstack/react-router";
import { ENVIRONMENT_INDEX_ROUTE_TO } from "../environment-route-paths";

type EnvironmentRouteParams = {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
};

type UseCanvasHotkeysInput = {
  params: EnvironmentRouteParams;
  selectedNodeId: string | null;
};

export function useCanvasEscapeShortcut({
  params,
  selectedNodeId,
}: UseCanvasHotkeysInput) {
  const navigate = useNavigate();

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented) {
        return;
      }

      const key = event.key.toLowerCase();

      if (
        selectedNodeId == null ||
        key !== "escape" ||
        event.ctrlKey ||
        event.shiftKey ||
        event.altKey ||
        event.metaKey
      ) {
        return;
      }

      event.preventDefault();
      void navigate({
        to: ENVIRONMENT_INDEX_ROUTE_TO,
        params: {
          organizationSlug: params.organizationSlug,
          projectSlug: params.projectSlug,
          environmentSlug: params.environmentSlug,
        },
        search: (prev) => prev,
        viewTransition: { types: ["canvas-inspector-close"] },
      });
    }

    window.addEventListener("keydown", handleKeyDown);

    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [
    navigate,
    params.environmentSlug,
    params.organizationSlug,
    params.projectSlug,
    selectedNodeId,
  ]);
}
