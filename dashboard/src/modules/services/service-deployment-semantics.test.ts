import { describe, expect, it } from "vitest";
import { getServiceDeploymentSemantics } from "#/modules/services/service-deployment-semantics";

const deployed = {
  isEmpty: false,
  hasBeenDeployed: true,
  currentDiffRowCount: 0,
  hasRecordedTargetSnapshot: true,
  latestDeploymentStatus: "applied" as const,
  missingLiveValues: [],
  off: false,
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
      state: undefined,
      statusText: "Deploying…",
      showNewBadge: false,
    });
  });

  it("shows authored changes after a recorded target", () => {
    expect(
      getServiceDeploymentSemantics({
        ...deployed,
        currentDiffRowCount: 2,
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
        latestDeploymentStatus: null,
      }),
    ).toMatchObject({ statusText: "Empty" });
    expect(
      getServiceDeploymentSemantics({
        ...deployed,
        latestDeploymentStatus: null,
      }),
    ).toMatchObject({ statusText: "Deployed" });
  });

  it("flags Live values the latest attempt deployed empty, naming them", () => {
    expect(
      getServiceDeploymentSemantics({ ...deployed, missingLiveValues: ["db.PLOYZ_PRIVATE_DOMAIN"] }),
    ).toEqual({
      state: "warning",
      statusText: "Missing db.PLOYZ_PRIVATE_DOMAIN",
      showNewBadge: false,
    });
  });

  it("shows Off while its Environment is shut down, whatever came before", () => {
    for (const latestDeploymentStatus of ["failed", "cancelled", "applied"] as const) {
      expect(getServiceDeploymentSemantics({ ...deployed, latestDeploymentStatus, off: true }))
        .toEqual({ state: undefined, statusText: "Off", showNewBadge: false });
    }
  });
});
