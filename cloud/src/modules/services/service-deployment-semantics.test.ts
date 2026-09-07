import { describe, expect, it } from "vitest";
import type { RuntimeServiceRecord } from "#/modules/runtime/runtime";
import { getServiceDeploymentSemantics as getServiceDeploymentSemanticsImpl } from "#/modules/services/service-deployment-semantics";

type SemanticInput = Parameters<typeof getServiceDeploymentSemanticsImpl>[0];
function getServiceDeploymentSemantics(
  input: Omit<SemanticInput, "hasRecordedTargetSnapshot"> &
    Partial<Pick<SemanticInput, "hasRecordedTargetSnapshot">>,
) {
  return getServiceDeploymentSemanticsImpl({
    ...input,
    hasRecordedTargetSnapshot: input.hasRecordedTargetSnapshot ?? false,
  });
}

function runtime(
  instanceCount: number,
  readyInstanceCount = instanceCount,
): RuntimeServiceRecord {
  return {
    id: "prod:web",
    namespaceId: "prod",
    serviceId: "web",
    activeRevisionId: "rev-a",
    routeCount: 0,
    instanceCount,
    readyInstanceCount,
    bindings: [],
    updatedAt: new Date(0).toISOString(),
  };
}

const baseDeployedInput = {
  isEmpty: false,
  hasBeenDeployed: true,
  currentDiffRowCount: 0,
  latestDeploymentDiffRowCount: 1,
  hasRecordedTargetSnapshot: true,
  latestDeploymentStatus: "applied" as const,
  clusterStatus: "live" as const,
};

function state(input: Parameters<typeof getServiceDeploymentSemantics>[0]) {
  return getServiceDeploymentSemantics(input).state;
}

function statusText(
  input: Parameters<typeof getServiceDeploymentSemantics>[0],
) {
  return getServiceDeploymentSemantics(input).statusText;
}

describe("getServiceDeploymentSemantics", () => {
  it("treats a saved target as recorded without inventing deploy state", () => {
    const semantics = getServiceDeploymentSemantics({
      isEmpty: false,
      hasBeenDeployed: false,
      currentDiffRowCount: 0,
      latestDeploymentDiffRowCount: 0,
      hasRecordedTargetSnapshot: true,
      latestDeploymentStatus: null,
      runtime: null,
      clusterStatus: "live",
    });
    expect(semantics).toEqual({
      state: undefined,
      statusText: "Service is offline",
      showNewBadge: false,
    });
  });

  it("flags failed deploys with red when the attempt changes deployed state", () => {
    expect(
      state({
        ...baseDeployedInput,
        latestDeploymentStatus: "failed",
        runtime: runtime(1),
      }),
    ).toBe("destructive");
  });

  it("flags missing-from-cluster on a deployed non-empty service as red", () => {
    expect(
      state({
        ...baseDeployedInput,
        latestDeploymentDiffRowCount: 0,
        runtime: null,
      }),
    ).toBe("destructive");
  });

  it("does not let active deployment evidence hide confirmed runtime absence", () => {
    expect(
      statusText({
        ...baseDeployedInput,
        latestDeploymentStatus: "deploying",
        runtime: null,
      }),
    ).toBe("Missing from cluster");
  });

  it("does not flag missing-from-cluster while runtime services are loading", () => {
    const semantics = getServiceDeploymentSemantics({
      ...baseDeployedInput,
      latestDeploymentDiffRowCount: 0,
      runtime: null,
      runtimeIsLoading: true,
    });

    expect(semantics.state).toBeUndefined();
    expect(semantics.statusText).toBe("Connecting…");
  });

  it("flags expected-but-zero replicas as red", () => {
    expect(
      state({
        ...baseDeployedInput,
        latestDeploymentDiffRowCount: 0,
        runtime: runtime(0),
      }),
    ).toBe("destructive");
  });

  it("treats applying changed services as blue", () => {
    expect(
      state({
        ...baseDeployedInput,
        latestDeploymentStatus: "deploying",
        runtime: runtime(1),
      }),
    ).toBe("changed");
  });

  it("does not say an unchanged service is applying during an environment deploy", () => {
    const input = {
      ...baseDeployedInput,
      latestDeploymentStatus: "deploying" as const,
      latestDeploymentDiffRowCount: 0,
      runtime: runtime(1),
    };

    expect(state(input)).toBeUndefined();
    expect(statusText(input)).toBe("1 replica");
  });

  it("shows confirmed runtime absence before active deployment evidence", () => {
    const semantics = getServiceDeploymentSemantics({
      ...baseDeployedInput,
      latestDeploymentStatus: "deploying",
      latestDeploymentDiffRowCount: 0,
      currentDiffRowCount: 1,
      runtime: null,
    });

    expect(semantics.state).toBe("destructive");
    expect(semantics.statusText).toBe("Missing from cluster");
  });

  it("treats unshipped changes as blue", () => {
    expect(
      state({
        ...baseDeployedInput,
        latestDeploymentDiffRowCount: 0,
        currentDiffRowCount: 3,
        runtime: runtime(1),
      }),
    ).toBe("changed");
  });

  it("treats never-attempted services as green", () => {
    const semantics = getServiceDeploymentSemantics({
      ...baseDeployedInput,
      hasRecordedTargetSnapshot: false,
      hasBeenDeployed: false,
      currentDiffRowCount: 4,
      latestDeploymentDiffRowCount: 0,
      latestDeploymentStatus: null,
      clusterStatus: "error",
      runtime: null,
    });

    expect(semantics.state).toBe("success");
    expect(semantics.showNewBadge).toBe(true);
  });

  it("treats saved empty services as approved empty state", () => {
    const semantics = getServiceDeploymentSemantics({
      ...baseDeployedInput,
      isEmpty: true,
      hasBeenDeployed: false,
      currentDiffRowCount: 0,
      latestDeploymentDiffRowCount: 0,
      latestDeploymentStatus: null,
      runtime: null,
    });

    expect(semantics.state).toBeUndefined();
    expect(semantics.statusText).toBe("Empty");
    expect(semantics.showNewBadge).toBe(false);
  });

  it("does not treat saved never-deployed image services as new", () => {
    const semantics = getServiceDeploymentSemantics({
      ...baseDeployedInput,
      hasBeenDeployed: false,
      currentDiffRowCount: 0,
      latestDeploymentDiffRowCount: 0,
      latestDeploymentStatus: null,
      runtime: null,
    });

    expect(semantics.state).toBeUndefined();
    expect(semantics.statusText).toBe("Service is offline");
    expect(semantics.showNewBadge).toBe(false);
  });

  it("shows edits after a saved snapshot as normal changes", () => {
    const semantics = getServiceDeploymentSemantics({
      ...baseDeployedInput,
      hasBeenDeployed: false,
      currentDiffRowCount: 2,
      latestDeploymentDiffRowCount: 0,
      latestDeploymentStatus: null,
      runtime: null,
    });

    expect(semantics.state).toBe("changed");
    expect(semantics.statusText).toBe("2 changes");
    expect(semantics.showNewBadge).toBe(false);
  });

  it("does not leak cluster errors into never-attempted services", () => {
    expect(
      statusText({
        ...baseDeployedInput,
        hasRecordedTargetSnapshot: false,
        hasBeenDeployed: false,
        latestDeploymentDiffRowCount: 0,
        latestDeploymentStatus: null,
        clusterStatus: "error",
        runtime: null,
      }),
    ).toBe("Service will be created");
  });

  it("does not leak connecting state into never-attempted services", () => {
    expect(
      statusText({
        ...baseDeployedInput,
        hasRecordedTargetSnapshot: false,
        hasBeenDeployed: false,
        latestDeploymentDiffRowCount: 0,
        latestDeploymentStatus: null,
        clusterStatus: "connecting",
        runtime: null,
      }),
    ).toBe("Service will be created");
  });

  it("shows empty for applied empty services even when the cluster is unreachable", () => {
    expect(
      statusText({
        ...baseDeployedInput,
        isEmpty: true,
        latestDeploymentDiffRowCount: 0,
        clusterStatus: "error",
        runtime: null,
      }),
    ).toBe("Empty");
  });

  it("still shows cluster errors for deployed image services", () => {
    expect(
      statusText({
        ...baseDeployedInput,
        latestDeploymentDiffRowCount: 0,
        clusterStatus: "error",
        runtime: null,
      }),
    ).toBe("Cluster unreachable");
  });

  it("still shows connecting for deployed image services", () => {
    expect(
      statusText({
        ...baseDeployedInput,
        latestDeploymentDiffRowCount: 0,
        clusterStatus: "connecting",
        runtime: null,
      }),
    ).toBe("Connecting…");
  });

  it("treats cancelled-only services as never attempted", () => {
    const semantics = getServiceDeploymentSemantics({
      ...baseDeployedInput,
      hasRecordedTargetSnapshot: false,
      hasBeenDeployed: false,
      latestDeploymentDiffRowCount: 0,
      latestDeploymentStatus: "cancelled",
      runtime: null,
    });

    expect(semantics.state).toBe("success");
    expect(semantics.statusText).toBe("Service will be created");
    expect(semantics.showNewBadge).toBe(true);
  });

  it("treats edits after a cancelled deploy as blue changes", () => {
    const semantics = getServiceDeploymentSemantics({
      ...baseDeployedInput,
      hasBeenDeployed: false,
      currentDiffRowCount: 4,
      latestDeploymentDiffRowCount: 0,
      latestDeploymentStatus: "cancelled",
      runtime: null,
    });

    expect(semantics.state).toBe("changed");
    expect(semantics.statusText).toBe("4 changes");
    expect(semantics.showNewBadge).toBe(false);
  });

  it("warns when not all replicas are ready", () => {
    expect(
      state({
        ...baseDeployedInput,
        latestDeploymentDiffRowCount: 0,
        runtime: runtime(2, 1),
      }),
    ).toBe("warning");
  });

  it("stays neutral when all replicas are ready", () => {
    expect(
      state({
        ...baseDeployedInput,
        latestDeploymentDiffRowCount: 0,
        runtime: runtime(2),
      }),
    ).toBeUndefined();
  });

  it("ignores runtime checks for empty services", () => {
    expect(
      state({
        ...baseDeployedInput,
        isEmpty: true,
        latestDeploymentDiffRowCount: 0,
        runtime: null,
      }),
    ).toBeUndefined();
  });

  it("stays neutral when the cluster is not live", () => {
    expect(
      state({
        ...baseDeployedInput,
        clusterStatus: "connecting",
        latestDeploymentDiffRowCount: 0,
        runtime: null,
      }),
    ).toBeUndefined();
  });

  it("treats edits after a failed deploy as blue changes", () => {
    expect(
      state({
        ...baseDeployedInput,
        currentDiffRowCount: 4,
        latestDeploymentDiffRowCount: 0,
        latestDeploymentStatus: "failed",
        runtime: runtime(1),
      }),
    ).toBe("changed");
  });

  it("treats edits after starting a deploy as blue changes", () => {
    expect(
      state({
        ...baseDeployedInput,
        currentDiffRowCount: 4,
        latestDeploymentDiffRowCount: 0,
        latestDeploymentStatus: "deploying",
        runtime: runtime(1),
      }),
    ).toBe("changed");
  });
});
