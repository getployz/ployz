import { describe, expect, it } from "vitest";
import { findUnpullableSdkDeployImages } from "#/modules/deployments/image-gate";
import {
  createEmptyServiceSource,
  createGitServiceSource,
  createImageServiceSource,
} from "#/modules/environment-design/services";

const gitApi = {
  id: "service-api",
  name: "api",
  source: createGitServiceSource({
    repository: "acme/api",
    repositoryId: 42,
    installationId: 7,
  }),
};

describe("SDK deploy image gate", () => {
  it("fails git-as-source before any image is invented from the repo", () => {
    const error = findUnpullableSdkDeployImages([gitApi]);

    expect(error?._tag).toBe("DeployImageNotPullableError");
    expect(error?.serviceIds).toEqual(["service-api"]);
    expect(error?.message).toBe(
      "Deploy needs a pullable image (name, tag, or digest) for api. Git sources cannot be sent to the runtime yet.",
    );
    expect(JSON.stringify(error)).not.toContain("ployz.local");
  });

  it("fails when any service is still git even if others already have images", () => {
    const error = findUnpullableSdkDeployImages([
      {
        id: "service-web",
        name: "web",
        source: createImageServiceSource({ image: "nginx:1.27" }),
      },
      gitApi,
    ]);

    expect(error?.serviceIds).toEqual(["service-api"]);
  });

  it("allows Deploy when every service is a name, tag, or digest image", () => {
    const error = findUnpullableSdkDeployImages([
      {
        id: "service-name",
        name: "cache",
        source: createImageServiceSource({ image: "redis" }),
      },
      {
        id: "service-tag",
        name: "web",
        source: createImageServiceSource({ image: "nginx:1.27" }),
      },
      {
        id: "service-digest",
        name: "api",
        source: createImageServiceSource({
          image:
            "ghcr.io/acme/api@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        }),
      },
    ]);

    expect(error).toBeNull();
  });

  it("lists every blocking git service", () => {
    const error = findUnpullableSdkDeployImages([
      gitApi,
      {
        id: "service-worker",
        name: "worker",
        source: createGitServiceSource({
          repository: "acme/worker",
          repositoryId: 43,
          installationId: 7,
        }),
      },
    ]);

    expect(error?.serviceIds).toEqual(["service-api", "service-worker"]);
    expect(error?.message).toBe(
      "Deploy needs pullable images (name, tag, or digest) for api, worker. Git sources cannot be sent to the runtime yet.",
    );
  });

  it("does not block empty services that the SDK deploy omits", () => {
    const error = findUnpullableSdkDeployImages([
      {
        id: "service-empty",
        name: "placeholder",
        source: createEmptyServiceSource(),
      },
      {
        id: "service-web",
        name: "web",
        source: createImageServiceSource({ image: "nginx:1.27" }),
      },
    ]);

    expect(error).toBeNull();
  });
});
