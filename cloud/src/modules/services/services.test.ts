import { environmentDesignFields } from "#/modules/environment-design/fields";
import {
  decodeStrict,
  isValid,
} from "#/modules/environment-design/schema";
import { describe, expect, it } from "vitest";
import {
  createDefaultServiceHealthcheck,
  createDefaultServiceRestartPolicy,
  createEmptyServiceSource,
  createGitServiceSource,
  createImageServiceSource,
  createServiceSchema,
  detectRegistryCredentialProvider,
  getDefaultRegistryCredentialUsername,
  getRegistryHostFromImageReference,
  getServiceSourceName,
  normalizeRegistryCredentialUsername,
  projectServiceDeploymentConfig,
  servicePrivateDnsSchema,
  serviceInsertSchema,
  serviceRootDirSchema,
  serviceSourceSchema,
  updateServiceSchema,
} from "#/modules/environment-design/services";

describe("service schemas", () => {
  it("derives service names from repositories and images", () => {
    expect(
      getServiceSourceName(
        createGitServiceSource({
          repository: "acme/api",
          repositoryId: 42,
          installationId: 7,
        })
      )
    ).toBe("api");
    expect(
      getServiceSourceName(
        createImageServiceSource({ image: "ghcr.io/acme/worker:latest" })
      )
    ).toBe("worker");
    expect(
      getServiceSourceName(
        createImageServiceSource({ image: "ghcr.io/acme/worker@sha256:abc" })
      )
    ).toBe("worker");
    expect(getServiceSourceName(createEmptyServiceSource())).toBeNull();
  });

  it("requires a runtime-safe private DNS service ID", () => {
    expect(decodeStrict(servicePrivateDnsSchema, "api_internal-1")).toBe(
      "api_internal-1",
    );
    expect(isValid(servicePrivateDnsSchema, null)).toBe(false);
    expect(isValid(servicePrivateDnsSchema, "")).toBe(false);
    expect(isValid(servicePrivateDnsSchema, "api.internal")).toBe(false);
  });

  it("update schema rejects generated columns", () => {
    const result = isValid(updateServiceSchema, {
      id: crypto.randomUUID(),
      organizationSlug: "org",
      environmentId: crypto.randomUUID(),
      serviceId: crypto.randomUUID(),
      name: "api",
      source: createGitServiceSource({
        repository: "acme/api",
        repositoryId: 42,
        installationId: 7,
      }),
      preDeployCommand: null,
      startCommand: null,
      healthcheck: {
        type: "none",
      },
      restartPolicy: "unless-stopped",
    });

    expect(result).toBe(false);
  });

  it("service select schema preserves field-level validation", () => {
    expect(decodeStrict(environmentDesignFields.service.name, "  api  ")).toBe("api");
    expect(
      isValid(environmentDesignFields.service.name, " ".repeat(65))
    ).toBe(false);
    expect(
      decodeStrict(serviceSourceSchema, {
        version: 2,
        type: "git",
        repository: "acme/api",
        repositoryId: 42,
        installationId: 7,
        rootDir: "/apps/api/",
        branch: {
          type: "connected",
          name: "main",
        },
        autoDeploy: true,
        waitForCi: true,
      })
    ).toEqual({
      version: 2,
      type: "git",
      repository: "acme/api",
      repositoryId: 42,
      installationId: 7,
      rootDir: "/apps/api",
      branch: {
        type: "connected",
        name: "main",
      },
      autoDeploy: true,
      waitForCi: true,
    });
    expect(isValid(serviceRootDirSchema, "/apps/api")).toBe(true);
    expect(isValid(serviceRootDirSchema, "/apps/api two")).toBe(false);
    expect(isValid(serviceRootDirSchema, "/apps//api")).toBe(false);
    expect(
      isValid(environmentDesignFields.service.preDeployCommand, "   ")
    ).toBe(false);
    expect(
      isValid(environmentDesignFields.service.healthcheck, {
        type: "http",
        path: "/health",
        timeoutSeconds: 30,
      })
    ).toBe(true);
    expect(
      isValid(environmentDesignFields.service.healthcheck, {
        type: "http",
        path: "health",
        timeoutSeconds: 30,
      })
    ).toBe(false);
    expect(
      isValid(environmentDesignFields.service.restartPolicy, "sometimes")
    ).toBe(false);
  });

  it("service insert schema accepts the intended db payload", () => {
    const result = isValid(serviceInsertSchema, {
      organizationId: crypto.randomUUID(),
      projectId: crypto.randomUUID(),
      environmentId: crypto.randomUUID(),
      lineageId: crypto.randomUUID(),
      name: "api",
      slug: "api",
      privateDns: "api",
      sourceType: "git",
      sourceConfig: {
        version: 2,
        type: "git",
        repository: "acme/api",
        repositoryId: 42,
        installationId: 7,
        rootDir: "/",
        branch: {
          type: "connected",
          name: "main",
        },
        autoDeploy: true,
        waitForCi: false,
      },
      preDeployCommand: null,
      startCommand: "npm start",
      healthcheck: {
        type: "http",
        path: "/health",
        timeoutSeconds: 30,
      },
      restartPolicy: "unless-stopped",
    });

    expect(result).toBe(true);
  });

  it("create schema parses the current request shape", () => {
    const createResult = decodeStrict(createServiceSchema, {
      organizationSlug: "org",
      environmentId: crypto.randomUUID(),
      x: 10,
      y: 20,
      name: "api",
      source: {
        version: 2,
        type: "git",
        repository: "acme/api",
        repositoryId: 42,
        installationId: 7,
        rootDir: "/",
        branch: {
          type: "disconnected",
          previousName: "main",
        },
        autoDeploy: true,
        waitForCi: false,
      },
    });

    expect(createResult).toMatchObject({
      preDeployCommand: null,
      startCommand: null,
      healthcheck: createDefaultServiceHealthcheck(),
      restartPolicy: createDefaultServiceRestartPolicy(),
    });
    expect(
      isValid(updateServiceSchema, {
        organizationSlug: "org",
        environmentId: crypto.randomUUID(),
        serviceId: crypto.randomUUID(),
        name: " ".repeat(65),
        source: {
          version: 1,
          type: "empty",
          rootDir: "/",
        },
        preDeployCommand: null,
        startCommand: null,
        healthcheck: {
          type: "none",
        },
        restartPolicy: "unless-stopped",
      })
    ).toBe(false);
  });

  it("defaults optional deployment fields omitted by historical projections", () => {
    const config = projectServiceDeploymentConfig({
      name: "api",
      source: createEmptyServiceSource(),
      preDeployCommand: null,
      startCommand: null,
      healthcheck: createDefaultServiceHealthcheck(),
      restartPolicy: createDefaultServiceRestartPolicy(),
      privateDns: "api",
    });

    expect(config).toMatchObject({
      maxRetries: 10,
      cron: null,
      replicas: 1,
      cpuLimit: null,
      memLimit: null,
      routes: [],
      managedHostname: null,
      build: {
        builder: "auto",
        dockerfilePath: null,
        watchPaths: [],
      },
    });
  });

  it("detects docker hub shorthand and known registry hosts", () => {
    expect(getRegistryHostFromImageReference("hello-world")).toBe("docker.io");
    expect(
      getRegistryHostFromImageReference("username/private-image:latest")
    ).toBe("docker.io");
    expect(
      getRegistryHostFromImageReference("ghcr.io/username/repo:latest")
    ).toBe("ghcr.io");
    expect(
      getRegistryHostFromImageReference("registry.gitlab.com/group/repo:tag")
    ).toBe("registry.gitlab.com");
    expect(getRegistryHostFromImageReference("quay.io/org/repo:tag")).toBe(
      "quay.io"
    );
    expect(
      getRegistryHostFromImageReference("public.ecr.aws/org/repo:tag")
    ).toBe("public.ecr.aws");
    expect(
      getRegistryHostFromImageReference(
        "us-west1-docker.pkg.dev/project/repo/image:tag"
      )
    ).toBe("us-west1-docker.pkg.dev");
    expect(
      getRegistryHostFromImageReference("mcr.microsoft.com/username/repo:tag")
    ).toBe("mcr.microsoft.com");
  });

  it("falls back to custom registries for arbitrary docker-auth hosts", () => {
    expect(
      getRegistryHostFromImageReference("registry.example.com/team/api:latest")
    ).toBe("registry.example.com");
    expect(
      getRegistryHostFromImageReference("registry.example.com:5000/team/api:v1")
    ).toBe("registry.example.com:5000");
    expect(
      detectRegistryCredentialProvider("registry.example.com/team/api:latest")
    ).toBe("custom");
  });

  it("maps providers from image references", () => {
    expect(detectRegistryCredentialProvider("hello-world")).toBe("docker-hub");
    expect(
      detectRegistryCredentialProvider("ghcr.io/username/repo:latest")
    ).toBe("ghcr");
    expect(
      detectRegistryCredentialProvider("registry.gitlab.com/group/repo:tag")
    ).toBe("gitlab");
    expect(detectRegistryCredentialProvider("quay.io/org/repo:tag")).toBe(
      "quay"
    );
    expect(
      detectRegistryCredentialProvider("public.ecr.aws/org/repo:tag")
    ).toBe("aws-ecr-public");
    expect(
      detectRegistryCredentialProvider(
        "us-west1-docker.pkg.dev/project/repo/image:tag"
      )
    ).toBe("gcp-artifact-registry");
    expect(
      detectRegistryCredentialProvider("mcr.microsoft.com/username/repo:tag")
    ).toBe("mcr");
  });

  it("applies fixed username rules and token-only GHCR behavior", () => {
    expect(getDefaultRegistryCredentialUsername("aws-ecr-public")).toBe("AWS");
    expect(getDefaultRegistryCredentialUsername("gcp-artifact-registry")).toBe(
      "_json_key"
    );
    expect(getDefaultRegistryCredentialUsername("ghcr")).toBeNull();

    expect(
      normalizeRegistryCredentialUsername({
        provider: "aws-ecr-public",
        username: "anything",
      })
    ).toBe("AWS");
    expect(
      normalizeRegistryCredentialUsername({
        provider: "gcp-artifact-registry",
        username: "anything",
      })
    ).toBe("_json_key");
    expect(
      normalizeRegistryCredentialUsername({
        provider: "ghcr",
        username: "ignored",
      })
    ).toBeNull();
    expect(
      normalizeRegistryCredentialUsername({
        provider: "docker-hub",
        username: "nick",
      })
    ).toBe("nick");
    expect(() =>
      normalizeRegistryCredentialUsername({
        provider: "docker-hub",
        username: "",
      })
    ).toThrow();
  });
});
