import { useCollectionScope } from "#/collections/use-collection-scope";
import { useRef, useState } from "react";
import { useReactFlow } from "@xyflow/react";
import { useLoaderData, useNavigate } from "@tanstack/react-router";
import type { EnvironmentRef } from "@ployz/sdk";
import { createServiceCommand, newServiceName, serviceName, uniqueName, type NewServiceSource } from "#/modules/config-store/store-services";
import { databaseCommand, randomPassword, type DatabasePreset } from "#/modules/config-store/database-presets";
import { servicesQuery, storeViewOptions, volumesQuery } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { usePlaceNewNode } from "./useCanvasPositionMutation";
import { SERVICE_NODE_HEIGHT, SERVICE_NODE_SIZE, SNAP_GRID } from "./constants";
import type { CreatePanel } from "#/components/create-menu-items";
import type { CanvasResourceNode, FlowPosition } from "./types";
import { findPlacement } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-utils/node-placement";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../environment-route-paths";

/** Where a new Service goes: its Environment, and where the canvas put it. */
export type NewServicePlacement = { store: EnvironmentRef; environmentId: string; position: FlowPosition };

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
  return (target: NewServicePlacement, source: NewServiceSource) => {
    const id = crypto.randomUUID();
    place({ environmentId: target.environmentId, resourceType: "service", resourceId: id, ...target.position });
    const name = newServiceName(source, services(target.store));
    const { isPersisted } = writer.commit(createServiceCommand(id, target.store, name, source));
    return { service: { id }, persisted: isPersisted.promise };
  };
}

/** Creates a Database Preset's Service, as `useCreateStoreService` does, with its Volume placed below it: one Batch. */
export function useCreateStoreDatabase(organizationSlug: string) {
  const { writer, place, services, volumeNames } = useStoreCreate(organizationSlug);
  return (target: NewServicePlacement, preset: DatabasePreset) => {
    const service = crypto.randomUUID();
    const volume = crypto.randomUUID();
    const { x, y } = target.position;
    place({ environmentId: target.environmentId, resourceType: "service", resourceId: service, x, y });
    place({ environmentId: target.environmentId, resourceType: "volume", resourceId: volume, x, y: y + SERVICE_NODE_HEIGHT + SNAP_GRID[1] * 2 });
    const name = serviceName(preset.id, services(target.store));
    const command = databaseCommand(preset, { service, volume, environment: target.store, name,
      volumeName: uniqueName(`${name}-data`, volumeNames(target.store)), password: randomPassword() });
    const { isPersisted } = writer.commit(command);
    return { service: { id: service }, persisted: isPersisted.promise };
  };
}

export function useServiceCreator(
  params: {
    organizationSlug: string;
    projectSlug: string;
    environmentSlug: string;
  },
  environmentId: string,
  getViewportCenter: () => FlowPosition,
) {
  const navigate = useNavigate();
  const flow = useReactFlow<CanvasResourceNode>();
  const createStoreService = useCreateStoreService(params.organizationSlug);
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });

  const [creatorOpen, setCreatorOpen] = useState(false);
  const [creatorPosition, setCreatorPosition] = useState<FlowPosition>({
    x: 0,
    y: 0,
  });
  const [creatorPanel, setCreatorPanel] = useState<CreatePanel>("root");
  const lastRightClickFlowPosition = useRef<FlowPosition>({ x: 0, y: 0 });

  function computePlacement(target: FlowPosition) {
    const existingRects = flow.getNodes().map((node) => ({
      x: node.position.x,
      y: node.position.y,
      ...SERVICE_NODE_SIZE,
    }));
    return findPlacement(target, SERVICE_NODE_SIZE, existingRects);
  }

  function openCreator(position: FlowPosition, panel: CreatePanel = "root") {
    setCreatorPosition(computePlacement(position));
    setCreatorPanel(panel);
    setCreatorOpen(true);
  }

  function openCreatorAtCenter(panel: CreatePanel = "root") {
    openCreator(getViewportCenter(), panel);
  }

  async function createBlankService(position: FlowPosition) {
    const placement = computePlacement(position);
    const created = createStoreService({ store, environmentId, position: placement }, { type: "empty" });
    await navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: created.service.id } });
  }

  function onPaneContextMenu(event: MouseEvent | React.MouseEvent) {
    lastRightClickFlowPosition.current = flow.screenToFlowPosition({
      x: event.clientX,
      y: event.clientY,
    });
  }

  function openCreatorAtLastRightClick(panel: CreatePanel) {
    openCreator(lastRightClickFlowPosition.current, panel);
  }

  function createBlankServiceAtLastRightClick() {
    void createBlankService(lastRightClickFlowPosition.current).catch((error) =>
      toast.error(toErrorMessage(error, "The service couldn’t be created. Try again.")),
    );
  }

  return {
    creatorOpen,
    setCreatorOpen,
    creatorPosition,
    creatorPanel,
    openCreatorAtCenter,
    onPaneContextMenu,
    openCreatorAtLastRightClick,
    createBlankServiceAtLastRightClick,
  };
}
