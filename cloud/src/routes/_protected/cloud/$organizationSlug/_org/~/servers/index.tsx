import { useState } from "react";
import { createFileRoute } from "@tanstack/react-router";
import { ServerIcon } from "lucide-react";
import { DashboardPage } from "#/components/dashboard-page";
import { ResourcePageControls } from "#/components/resource-page-controls";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "#/components/ui/empty";
import { preloadRuntimeCollections } from "#/modules/runtime/runtime.collection";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { AddServerDialog } from "./-components/add-server-dialog";
import { RuntimeMachineRow } from "./-components/server-list-rows";
import { ServersSkeleton } from "./-components/servers-skeleton";
import {
  getServerListState,
  incompleteRuntimeObservationDescription,
} from "./-components/server-list-state";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/",
)({
  loader: async ({ params }) => {
    await preloadRuntimeCollections({
      organizationSlug: params.organizationSlug,
    });
  },
  pendingComponent: ServersSkeleton,
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const [query, setQuery] = useState("");
  const runtime = useRuntimeLens(organizationSlug);
  const runtimeMachineRows = runtime.machines;
  const normalizedQuery = query.trim().toLowerCase();

  const filtered = normalizedQuery
    ? runtimeMachineRows.filter((machine) => {
        const haystack = [
          machine.id,
          machine.name,
          machine.publicIp,
          ...machine.endpoints,
        ]
          .filter(Boolean)
          .join(" ")
          .toLowerCase();
        return haystack.includes(normalizedQuery);
      })
    : runtimeMachineRows;
  const runtimeStatus = runtime.status;
  const runtimeError = runtime.error;
  const incompleteObservation = incompleteRuntimeObservationDescription(
    runtime.incompleteIds,
  );
  const listState = getServerListState({
    rowCount: runtimeMachineRows.length,
    visibleRowCount: filtered.length,
    query,
    runtimeStatus,
    runtimeError,
  });

  return (
    <DashboardPage>
      <h1 className="sr-only">Servers</h1>
      <ResourcePageControls
        controlsLabel="Servers controls"
        searchAriaLabel="Search servers"
        searchPlaceholder="Search servers"
        searchValue={query}
        onSearchValueChange={setQuery}
        action={<AddServerDialog organizationSlug={organizationSlug} />}
      />

      {listState.notice ? (
        <Alert>
          <AlertTitle>{listState.notice.title}</AlertTitle>
          <AlertDescription>
            {listState.notice.description}
          </AlertDescription>
        </Alert>
      ) : null}
      {incompleteObservation ? (
        <Alert>
          <AlertTitle>
            {runtimeStatus === "unavailable"
              ? "Last observed Runtime Watch was incomplete"
              : "Runtime Watch observation is incomplete"}
          </AlertTitle>
          <AlertDescription>{incompleteObservation}</AlertDescription>
        </Alert>
      ) : null}

      {listState.kind === "loading" ? (
        <ServersSkeleton listOnly />
      ) : listState.kind === "empty" ? (
        <Empty variant={listState.variant}>
          <EmptyHeader>
            {listState.variant === "first-run" ? (
              <EmptyMedia variant="icon">
                <ServerIcon />
              </EmptyMedia>
            ) : null}
            <EmptyTitle>{listState.title}</EmptyTitle>
            <EmptyDescription>{listState.description}</EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : (
        <div className="flex flex-col gap-3">
          {filtered.map((machine) => (
            <RuntimeMachineRow
              key={machine.id}
              machine={machine}
              organizationSlug={organizationSlug}
            />
          ))}
        </div>
      )}
    </DashboardPage>
  );
}
