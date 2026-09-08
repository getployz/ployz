import { describe, expect, it } from "vitest";
import { getServiceDeploymentSemantics } from "#/modules/services/service-deployment-semantics";

const deployed = {
  isEmpty: false,
  hasBeenDeployed: true,
  currentDiffRowCount: 0,
  latestDeploymentDiffRowCount: 1,
  hasRecordedTargetSnapshot: true,
  latestDeploymentStatus: "applied" as const,
};

describe("getServiceDeploymentSemantics", () => {
  it("shows a failed Deployment Attempt without interpreting runtime state", () => {
    expect(
      getServiceDeploymentSemantics({
        ...deployed,
        latestDeploymentStatus: "failed",
      }),
    ).toEqual({
      state: "destructive",
      statusText: "Deploy failed",
      showNewBadge: false,
    });
  });

  it("shows an active Deployment Attempt when it changes the target", () => {
    expect(
      getServiceDeploymentSemantics({
        ...deployed,
        latestDeploymentStatus: "deploying",
      }),
    ).toEqual({
      state: "changed",
      statusText: "Deploying…",
      showNewBadge: false,
    });
  });

  it("shows authored changes after a recorded target", () => {
    expect(
      getServiceDeploymentSemantics({
        ...deployed,
        currentDiffRowCount: 2,
        latestDeploymentDiffRowCount: 0,
        latestDeploymentStatus: null,
      }),
    ).toEqual({
      state: "changed",
      statusText: "2 changes",
      showNewBadge: false,
    });
  });

  it("marks a never-attempted service as new", () => {
    expect(
      getServiceDeploymentSemantics({
        ...deployed,
        hasBeenDeployed: false,
        currentDiffRowCount: 0,
        latestDeploymentDiffRowCount: 0,
        hasRecordedTargetSnapshot: false,
        latestDeploymentStatus: null,
      }),
    ).toEqual({
      state: "success",
      statusText: "Service will be created",
      showNewBadge: true,
    });
  });

  it("keeps empty and deployed labels in authored/attempt history", () => {
    expect(
      getServiceDeploymentSemantics({
        ...deployed,
        isEmpty: true,
        latestDeploymentDiffRowCount: 0,
        latestDeploymentStatus: null,
      }),
    ).toMatchObject({ statusText: "Empty" });
    expect(
      getServiceDeploymentSemantics({
        ...deployed,
        latestDeploymentDiffRowCount: 0,
        latestDeploymentStatus: null,
      }),
    ).toMatchObject({ statusText: "Deployed" });
  });
});
