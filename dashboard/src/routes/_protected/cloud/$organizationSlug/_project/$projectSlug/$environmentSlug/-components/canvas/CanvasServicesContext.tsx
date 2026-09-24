import { createContext, use, type ReactNode } from "react";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import type { VolumeResourceRecord } from "#/modules/environment-design/resources";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";

export type CanvasServiceState = {
  serviceView: EnvironmentServiceViewRecord;
  diffRowCount: number;
  hasRecordedTargetSnapshot: boolean;
  latestDeploymentStatus: EnvironmentDeploymentStatus | null;
};

export type CanvasVolumeResourceState = {
  resource: VolumeResourceRecord;
  diffRowCount: number;
};

const CanvasServicesContext = createContext<Map<string, CanvasServiceState> | null>(
  null,
);
const CanvasVolumeResourcesContext =
  createContext<Map<string, CanvasVolumeResourceState> | null>(null);

export function CanvasServicesProvider({
  servicesById,
  volumeResourcesById,
  children,
}: {
  servicesById: Map<string, CanvasServiceState>;
  volumeResourcesById: Map<string, CanvasVolumeResourceState>;
  children: ReactNode;
}) {
  return (
    <CanvasServicesContext.Provider value={servicesById}>
      <CanvasVolumeResourcesContext.Provider value={volumeResourcesById}>
        {children}
      </CanvasVolumeResourcesContext.Provider>
    </CanvasServicesContext.Provider>
  );
}

export function useCanvasService(serviceId: string) {
  const servicesById = use(CanvasServicesContext);

  if (!servicesById) {
    throw new Error("CanvasServicesProvider is missing");
  }

  return servicesById.get(serviceId) ?? null;
}

export function useCanvasVolumeResource(resourceId: string) {
  const volumeResourcesById = use(CanvasVolumeResourcesContext);

  if (!volumeResourcesById) {
    throw new Error("CanvasVolumeResourcesContext is missing");
  }

  return volumeResourcesById.get(resourceId) ?? null;
}
