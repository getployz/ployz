import { useCollectionScope } from "#/collections/use-collection-scope";
import { useLoaderData, useNavigate, useRouter } from "@tanstack/react-router";
import type { EnvironmentRef } from "@ployz/sdk";
import { createServiceCommand, newServiceName, serviceName, uniqueName, type NewServiceSource } from "#/modules/config-store/store-services";
import { databaseCommand, type DatabasePreset } from "#/modules/config-store/database-presets";
import { servicesQuery, storeViewOptions, volumesQuery } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { usePlaceNewNode } from "../../$projectSlug/$environmentSlug/-components/canvas/useCanvasPositionMutation";
import { SERVICE_NODE_HEIGHT, SNAP_GRID } from "../../$projectSlug/$environmentSlug/-components/canvas/constants";
import type { FlowPosition } from "../../$projectSlug/$environmentSlug/-components/canvas/types";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO, type EnvironmentRouteParams } from "#/lib/environment-routes";

/** Where a new Service goes: its Environment, and where the canvas put it. */
export type NewServicePlacement = { store: EnvironmentRef; environmentId: string; position: FlowPosition };

/** A Service created on the canvas: its id at once, and the write that saves it. */
export type CreatedService = { service: { id: string }; persisted: Promise<unknown> };

/** What creating a Service needs: the writer, canvas placement, and the names this tab has seen taken. */
function useStoreCreate(organizationSlug: string) {
  const scope = useCollectionScope();
  const writer = useStoreWriter(organizationSlug);
  const place = usePlaceNewNode(organizationSlug);
  const cached = <Q extends Parameters<typeof storeViewOptions>[2]>(query: Q) =>
    scope.queryClient.getQueryData(storeViewOptions(organizationSlug, scope, query).queryKey);
  return {
    writer,
    place,
    services: (store: EnvironmentRef) => {
      const listed = cached(servicesQuery(store));
      return listed?.ok ? listed.value.services : [];
    },
    volumeNames: (store: EnvironmentRef) => {
      const listed = cached(volumesQuery(store));
      return listed?.ok ? listed.value.volumes.map((volume) => volume.name) : [];
    },
  };
}

/**
 * Creates a Service in the Config Store where the canvas put it, named from its source: on the canvas at once, saved
 * in the background. `persisted` settles once the Store has it, for callers whose next page can't show it before.
 */
export function useCreateStoreService(organizationSlug: string) {
  const { writer, place, services } = useStoreCreate(organizationSlug);
  return (target: NewServicePlacement, source: NewServiceSource): CreatedService => {
    const id = crypto.randomUUID();
    place({ environmentId: target.environmentId, resourceType: "service", resourceId: id, ...target.position });
    const name = newServiceName(source, services(target.store));
    return { service: { id }, persisted: writer.commit(createServiceCommand(id, target.store, name, source)) };
  };
}

/** Creates a Database Preset's Service, as `useCreateStoreService` does, with its Volume placed below it: one Batch. */
export function useCreateStoreDatabase(organizationSlug: string) {
  const { writer, place, services, volumeNames } = useStoreCreate(organizationSlug);
  return (target: NewServicePlacement, preset: DatabasePreset): CreatedService => {
    const service = crypto.randomUUID();
    const volume = crypto.randomUUID();
    const { x, y } = target.position;
    place({ environmentId: target.environmentId, resourceType: "service", resourceId: service, x, y });
    place({ environmentId: target.environmentId, resourceType: "volume", resourceId: volume, x, y: y + SERVICE_NODE_HEIGHT + SNAP_GRID[1] * 2 });
    const name = serviceName(preset.id, services(target.store));
    const command = databaseCommand(preset, { service, volume, environment: target.store, name,
      volumeName: uniqueName(`${name}-data`, volumeNames(target.store)) });
    return { service: { id: service }, persisted: writer.commit(command) };
  };
}

/**
 * Opens a new Service's panel at once, before the Store has it. A refusal undoes the Service, so whoever is still on
 * its panel goes back to the canvas, where the refusal's toast says why. The panel's entry becomes the canvas, and
 * when the panel opened straight from the canvas and is still that entry, history steps back onto the canvas it came
 * from, so neither Back nor Forward lands on a second canvas or the undone panel.
 */
export function useOpenCreatedService(params: EnvironmentRouteParams) {
  const navigate = useNavigate();
  const router = useRouter();
  return async ({ service, persisted }: CreatedService) => {
    const panel = { to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: service.id } } as const;
    const fromCanvas = router.matchRoute({ to: ENVIRONMENT_INDEX_ROUTE_TO, params }) !== false;
    await navigate({ ...panel, search: (prev) => prev });
    const opened = router.history.location.state.__TSR_index;
    persisted.catch(async () => {
      if (!router.matchRoute(panel, { fuzzy: true })) return;
      const stepBack = fromCanvas && router.history.location.state.__TSR_index === opened;
      await navigate({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, replace: true, search: (prev) => prev });
      // The replace keeps the index, so a Back or a new panel while it loaded moves it, and they already left.
      if (stepBack && router.history.location.state.__TSR_index === opened) router.history.back();
    });
  };
}

/** Creates an empty Service at a canvas position and opens its panel; a failure shows as a toast. */
export function useBlankServiceCreator(params: EnvironmentRouteParams, environmentId: string) {
  const openCreated = useOpenCreatedService(params);
  const createStoreService = useCreateStoreService(params.organizationSlug);
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });

  async function createBlankService(position: FlowPosition) {
    await openCreated(createStoreService({ store, environmentId, position }, { type: "empty" }));
  }

  return (position: FlowPosition) => {
    void createBlankService(position).catch((error) =>
      toast.error(toErrorMessage(error, "The service couldn’t be created. Try again.")),
    );
  };
}
