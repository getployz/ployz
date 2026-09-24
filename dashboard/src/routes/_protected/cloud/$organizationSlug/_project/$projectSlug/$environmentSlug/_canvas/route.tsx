import { Suspense } from "react";
import {
  createFileRoute,
  type ErrorComponentProps,
} from "@tanstack/react-router";
import { RouteErrorAlert } from "#/components/route-error-alert";
import {
  EnvironmentCanvasScene,
  PendingCanvas,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentCanvasScene";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas",
)({
  errorComponent: CanvasError,
  component: CanvasLayout,
});

function CanvasLayout() {
  return (
    <div className="h-full overflow-hidden">
      <Suspense fallback={<PendingCanvas />}>
        <EnvironmentCanvasScene />
      </Suspense>
    </div>
  );
}

function CanvasError({ error }: ErrorComponentProps) {
  return (
    <div className="flex h-full items-center justify-center p-6">
      <RouteErrorAlert
        title="Couldn’t load this environment"
        description={
          error.message || "The environment data could not be loaded."
        }
      />
    </div>
  );
}
