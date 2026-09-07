// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { EnvironmentChangeStateProjection } from "#/modules/deployments/deployment-contract";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";
import { projectServiceDeploymentConfig } from "#/modules/environment-design/services";
import {
  RUNTIME_PUBLIC_URL_NONE,
  type RuntimeSnapshotLens,
} from "#/modules/runtime/runtime.collection";
import {
  RuntimeProvider,
  useRuntimeServices,
  useRuntimeStatus,
} from "#/providers/runtime-provider";
import { useCanvasFlowState } from "./useCanvasFlowState";

let runtimeLensListener: ((event: MessageEvent) => void) | undefined;

class FakeEventSource {
  addEventListener(type: string, listener: (event: MessageEvent) => void) {
    if (type === "runtime.lens") runtimeLensListener = listener;
  }
  removeEventListener() {}
  close() {}
}

vi.stubGlobal("EventSource", FakeEventSource);

afterEach(() => {
  cleanup();
  runtimeLensListener = undefined;
});

function serviceView(): EnvironmentServiceViewRecord {
  return {
    service: {
      id: "service-1",
      environmentId: "environment-1",
      lineageId: "lineage-1",
      name: "api",
      slug: "api",
      source: {
        version: 1,
        type: "image",
        image: "docker.io/library/nginx:stable",
        autoUpdate: { type: "off" },
        credentials: { type: "none" },
      },
      registryCredentialUsername: null,
      hasStoredRegistryCredential: false,
      preDeployCommand: null,
      startCommand: null,
      healthcheck: { type: "none" },
      restartPolicy: "unless-stopped",
      maxRetries: 10,
      cron: null,
      replicas: 2,
      cpuLimit: null,
      memLimit: null,
      privateDns: "api",
      routes: [],
      managedHostname: null,
      build: { builder: "auto", dockerfilePath: null, watchPaths: [] },
      env: {},
      mounts: [],
      firstDeployedAt: new Date("2026-08-12T00:00:00.000Z"),
      deletedAt: null,
      createdAt: new Date("2026-08-12T00:00:00.000Z"),
      updatedAt: new Date("2026-08-12T00:00:00.000Z"),
      projectSlug: "cloud",
      environmentSlug: "production",
      $synced: true,
      $origin: "remote",
      $key: "service-1",
      $collectionId: "services",
    },
    canvasPositions: [],
    variables: [],
    variableGroupAttachments: [],
    volumeAttachments: [],
    $synced: true,
    $origin: "remote",
    $key: "service-1",
    $collectionId: "environment-services-view",
  } as EnvironmentServiceViewRecord;
}

describe("useCanvasFlowState runtime connection adapter", () => {
  it("recomputes drift after runtime reconnects without changing Saved or Applied", async () => {
    const view = serviceView();
    const config = projectServiceDeploymentConfig(view.service);
    const environmentChangeState = {
      environmentId: "environment-1",
      saved: {
        token: "saved-1",
        snapshotId: "snapshot-1",
        createdAt: new Date("2026-08-12T00:00:00.000Z"),
        nodes: [
          {
            nodeType: "service",
            nodeId: "service-1",
            nodeLineageId: "lineage-1",
            revisionId: "revision-2",
            config,
          },
        ],
      },
      applied: {
        token: "applied-1",
        nodes: [
          {
            nodeType: "service",
            nodeId: "service-1",
            nodeLineageId: "lineage-1",
            revisionId: "revision-2",
            config,
          },
        ],
      },
      deploymentEvidence: null,
    } satisfies EnvironmentChangeStateProjection;
    function Probe() {
      const { status } = useRuntimeStatus();
      const { runtimeServices, isLoading } = useRuntimeServices("production");
      const state = useCanvasFlowState({
        environmentNamespace: "production",
        servicesWithBoundEnv: [view],
        environmentResources: [],
        volumeResources: [],
        environmentChangeState,
        nodeIntroductions: [],
        runtimeStatus: status,
        runtimeServices,
        runtimeIsLoading: isLoading,
        autoDomain: null,
        canvasNodes: [],
        selectedNodeId: null,
      });
      return createElement("output", {
        "data-testid": "canvas-state",
        "data-status": status,
        "data-can-deploy": String(state.canDeploy),
        "data-unsaved": state.changeSlices.unsaved.totalCount,
        "data-pending": state.changeSlices.pending.totalCount,
        "data-drift": state.changeSlices.drift.totalCount,
        "data-drift-path": state.changeSlices.drift.groups[0]?.rows[0]?.path,
      });
    }

    render(
      createElement(
        RuntimeProvider,
        {
          organizationSlug: "acceptance-reconnect",
          children: createElement(Probe),
        },
      ),
    );

    const emitLens = (snapshot: RuntimeSnapshotLens) =>
      act(() => {
        runtimeLensListener?.({
          data: JSON.stringify(snapshot),
        } as MessageEvent);
      });
    const baseLens = {
      error: null,
      publicUrl: RUNTIME_PUBLIC_URL_NONE,
      machines: [],
      updatedAt: "2026-08-12T00:01:00.000Z",
    };

    emitLens({ ...baseLens, status: "no_connection", services: [] });
    await waitFor(() => {
      const state = screen.getByTestId("canvas-state");
      expect(state.getAttribute("data-status")).toBe("disabled");
      expect(state.getAttribute("data-can-deploy")).toBe("false");
      expect(state.getAttribute("data-drift")).toBe("0");
    });

    emitLens({
      ...baseLens,
      status: "live_rows",
      services: [
        {
          id: "runtime-api",
          namespaceId: "production",
          serviceId: "api",
          activeRevisionId: "revision-2",
          routeCount: 0,
          instanceCount: 1,
          readyInstanceCount: 1,
          bindings: [],
          updatedAt: "2026-08-12T00:01:00.000Z",
        },
      ],
    });
    await waitFor(() => {
      const state = screen.getByTestId("canvas-state");
      expect(state.getAttribute("data-status")).toBe("live");
      expect(state.getAttribute("data-can-deploy")).toBe("true");
      expect(state.getAttribute("data-drift")).toBe("1");
      expect(state.getAttribute("data-drift-path")).toBe("runtime.replicas");
      expect(state.getAttribute("data-unsaved")).toBe("0");
      expect(state.getAttribute("data-pending")).toBe("0");
    });
  });
});
