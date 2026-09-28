import { describe, expect, it } from "vitest";
import { createGitServiceSource, createImageServiceSource } from "#/modules/environment-design/services";
import { destinations, trackedBranches, type DestinationCandidate } from "./destinations";

const access = { type: "github-installation" as const, installationId: 7 };
const tracking = (name: string, repositoryId = 42) =>
  ({ source: createGitServiceSource({ repository: "acme/app", repositoryId, access, branch: { type: "connected", name } }) });
const env = (id: string, services: DestinationCandidate["savedServices"], parentId: string | null = null, prEnvironment = false): DestinationCandidate =>
  ({ id, parentId, prEnvironment, savedServices: services });

const project = [
  env("production", [tracking("main"), { source: createImageServiceSource({ image: "postgres:16" }) }]),
  env("staging", [tracking("dev")], "production"),
  env("pr-142", [tracking("feature")], "production", true),
  env("other-repo", [tracking("main", 99)]),
];
const into = (targetBranch: string, environments = project) => destinations({ environments, repositoryId: 42, targetBranch });

describe("destinations", () => {
  it("lands main in production and dev in staging", () => {
    expect(into("main")).toEqual(["production"]);
    expect(into("dev")).toEqual(["staging"]);
  });

  it("skips an Environment with one above it on the same branch", () => {
    const nested = [...project, env("new-branch", [tracking("main")], "production"), env("qa", [tracking("dev")], "staging"),
      env("hotfix", [tracking("main")], "staging")];
    expect(into("main", nested)).toEqual(["production"]);
    expect(into("dev", nested)).toEqual(["staging"]);
  });

  it("lands in every Environment with none above it on the branch", () => {
    expect(into("main", [...project, env("eu", [tracking("main")])])).toEqual(["production", "eu"]);
  });

  it("lands nowhere for a branch no Environment deploys, and never in a PR Environment", () => {
    expect(into("release")).toEqual([]);
    expect(into("feature")).toEqual([]);
  });

  it("lists the branches that have Destinations", () => {
    expect(trackedBranches(project, 42)).toEqual(["dev", "main"]);
  });
});
