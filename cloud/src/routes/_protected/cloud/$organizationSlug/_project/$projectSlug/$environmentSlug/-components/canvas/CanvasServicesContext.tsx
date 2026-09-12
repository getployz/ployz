import { createContext, use, type ReactNode } from "react";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import type {
  VariableGroupResourceRecord,
  VolumeResourceRecord,
} from "#/modules/environment-design/resources";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";

export type CanvasServiceState = {
  serviceView: EnvironmentServiceViewRecord;
  diffRowCount: number;
  hasRecordedTargetSnapshot: boolean;
  latestDeploymentStatus: EnvironmentDeploymentStatus | null;
};

export type CanvasEnvironmentResourceState = {
  resource: VariableGroupResourceRecord;
  diffRowCount: number;
};

export type CanvasVolumeResourceState = {
  resource: VolumeResourceRecord;
  diffRowCount: number;
};

const CanvasServicesContext = createContext<Map<string, CanvasServiceState> | null>(
  null,
);
const CanvasEnvironmentResourcesContext =
  createContext<Map<string, CanvasEnvironmentResourceState> | null>(null);
const CanvasVolumeResourcesContext =
  createContext<Map<string, CanvasVolumeResourceState> | null>(null);

export function CanvasServicesProvider({
  servicesById,
  environmentResourcesById,
  volumeResourcesById,
  children,
}: {
  servicesById: Map<string, CanvasServiceState>;
  environmentResourcesById: Map<string, CanvasEnvironmentResourceState>;
  volumeResourcesById: Map<string, CanvasVolumeResourceState>;
  children: ReactNode;
}) {
  return (
    <CanvasServicesContext.Provider value={servicesById}>
      <CanvasEnvironmentResourcesContext.Provider value={environmentResourcesById}>
        <CanvasVolumeResourcesContext.Provider value={volumeResourcesById}>
          {children}
        </CanvasVolumeResourcesContext.Provider>
      </CanvasEnvironmentResourcesContext.Provider>
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

export function useCanvasEnvironmentResource(resourceId: string) {
  const environmentResourcesById = use(CanvasEnvironmentResourcesContext);

  if (!environmentResourcesById) {
    throw new Error("CanvasEnvironmentResourcesContext is missing");
  }

  return environmentResourcesById.get(resourceId) ?? null;
}

export function useCanvasVolumeResource(resourceId: string) {
  const volumeResourcesById = use(CanvasVolumeResourcesContext);

  if (!volumeResourcesById) {
    throw new Error("CanvasVolumeResourcesContext is missing");
  }

  return volumeResourcesById.get(resourceId) ?? null;
}
