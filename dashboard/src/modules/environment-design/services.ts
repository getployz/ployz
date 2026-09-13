import type {
  ServiceHealthcheck as ServiceHealthcheckRecord,
  ServiceRestartPolicy as ServiceRestartPolicyRecord,
} from "#/modules/environment-design/tables";
import { decodeStrict } from "#/modules/environment-design/schema";
import {
  registryCredentialUsernameSchema,
  serviceDeploymentConfigSchema,
  serviceHealthcheckSchema,
  serviceRestartPolicySchema,
  serviceSourceSchema,
  type RegistryCredentialProvider,
  type ServiceDeployEnv,
  type ServiceDeployMount,
  type ServiceDeploymentConfig,
  type ServiceGitBranch,
  type ServiceImageAutoUpdate,
  type ServiceImageCredentials,
  type ServiceRecord,
  type ServiceSource,
} from "#/modules/environment-design/service-schemas";

export * from "#/modules/environment-design/service-schemas";

const SERVICE_DEPLOYMENT_FIELD_KEYS = [
  "name",
  "source",
  "preDeployCommand",
  "startCommand",
  "healthcheck",
  "restartPolicy",
  "privateDns",
] as const;

// Config fields added after v2 shipped. They are optional in the selection so
// existing projection callers keep compiling; `serviceDeploymentConfigSchema`
// fills any omitted field with its default. Callers that must preserve the
// persisted value (e.g. the deploy snapshot projection) pass them explicitly.
const SERVICE_DEPLOYMENT_OPTIONAL_FIELD_KEYS = [
  "maxRetries",
  "cron",
  "replicas",
  "cpuLimit",
  "memLimit",
  "routes",
  "managedHostname",
  "build",
] as const;

export type ServiceDeploymentFieldSelection = Pick<
  ServiceRecord,
  (typeof SERVICE_DEPLOYMENT_FIELD_KEYS)[number]
> &
  Partial<
    Pick<ServiceRecord, (typeof SERVICE_DEPLOYMENT_OPTIONAL_FIELD_KEYS)[number]>
  > & {
    deletedAt?: Date | null;
    env?: ServiceDeployEnv;
    mounts?: ServiceDeployMount[];
    variableGroupAttachments?: ServiceDeploymentConfig["variableGroupAttachments"];
  };

type ServiceDeploymentConfigProjection = Omit<
  ServiceDeploymentConfig,
  (typeof SERVICE_DEPLOYMENT_OPTIONAL_FIELD_KEYS)[number]
> &
  Partial<
    Pick<
      ServiceDeploymentConfig,
      (typeof SERVICE_DEPLOYMENT_OPTIONAL_FIELD_KEYS)[number]
    >
  >;

const dockerHubHosts = new Set(["docker.io", "index.docker.io"]);

export function createDefaultServiceHealthcheck(): ServiceHealthcheckRecord {
  return decodeStrict(serviceHealthcheckSchema, {
    type: "none",
  });
}

export function createDefaultServiceRestartPolicy(): ServiceRestartPolicyRecord {
  return decodeStrict(serviceRestartPolicySchema, "unless-stopped");
}

export function createEmptyServiceSource(rootDir = "/"): ServiceSource {
  return decodeStrict(serviceSourceSchema, {
    version: 1,
    type: "empty",
    rootDir,
  });
}

export function createGitServiceSource(input: {
  repository: string;
  repositoryId: number;
  installationId: number;
  rootDir?: string;
  branch?: ServiceGitBranch;
  autoDeploy?: boolean;
  waitForCi?: boolean;
}): ServiceSource {
  return decodeStrict(serviceSourceSchema, {
    version: 2,
    type: "git",
    repository: input.repository,
    repositoryId: input.repositoryId,
    installationId: input.installationId,
    rootDir: input.rootDir ?? "/",
    branch: input.branch ?? {
      type: "connected",
      name: "main",
    },
    autoDeploy: input.autoDeploy ?? true,
    waitForCi: input.waitForCi ?? false,
  });
}

export function createImageServiceSource(input: {
  image: string;
  autoUpdate?: ServiceImageAutoUpdate;
  credentials?: ServiceImageCredentials;
}): ServiceSource {
  return decodeStrict(serviceSourceSchema, {
    version: 1,
    type: "image",
    image: input.image,
    autoUpdate: input.autoUpdate ?? { type: "off" },
    credentials: input.credentials ?? { type: "none" },
  });
}

export function getServiceSourceName(source: ServiceSource) {
  if (source.type === "git") {
    return source.repository.split("/").at(-1) || null;
  }

  if (source.type === "image") {
    const image = source.image.split("/").at(-1) ?? source.image;
    return image.split(/[:@]/)[0] || null;
  }

  return null;
}

export function getRegistryHostFromImageReference(image: string) {
  const trimmed = image.trim();
  const firstSegment = trimmed.split("/")[0]?.toLowerCase() ?? "";

  if (
    firstSegment.includes(".") ||
    firstSegment.includes(":") ||
    firstSegment === "localhost"
  ) {
    return dockerHubHosts.has(firstSegment) ? "docker.io" : firstSegment;
  }

  return "docker.io";
}

export function detectRegistryCredentialProvider(
  image: string,
): RegistryCredentialProvider {
  const registryHost = getRegistryHostFromImageReference(image);

  if (registryHost === "ghcr.io") {
    return "ghcr";
  }

  if (registryHost === "registry.gitlab.com") {
    return "gitlab";
  }

  if (registryHost === "quay.io") {
    return "quay";
  }

  if (registryHost === "public.ecr.aws") {
    return "aws-ecr-public";
  }

  if (registryHost.endsWith(".pkg.dev")) {
    return "gcp-artifact-registry";
  }

  if (registryHost === "mcr.microsoft.com") {
    return "mcr";
  }

  if (registryHost === "docker.io") {
    return "docker-hub";
  }

  return "custom";
}

export function getDefaultRegistryCredentialUsername(
  provider: RegistryCredentialProvider,
) {
  if (provider === "aws-ecr-public") {
    return "AWS";
  }

  if (provider === "gcp-artifact-registry") {
    return "_json_key";
  }

  return null;
}

export function normalizeRegistryCredentialUsername(input: {
  provider: RegistryCredentialProvider;
  username?: string | null;
}) {
  const fixedUsername = getDefaultRegistryCredentialUsername(input.provider);

  if (fixedUsername) {
    return fixedUsername;
  }

  if (input.provider === "ghcr") {
    return null;
  }

  return decodeStrict(registryCredentialUsernameSchema, input.username ?? "");
}

export function getRegistryCredentialProviderLabel(
  provider: RegistryCredentialProvider,
) {
  switch (provider) {
    case "docker-hub":
      return "Docker Hub";
    case "ghcr":
      return "GitHub Container Registry";
    case "gitlab":
      return "GitLab Container Registry";
    case "quay":
      return "Quay.io";
    case "aws-ecr-public":
      return "AWS ECR Public";
    case "gcp-artifact-registry":
      return "Google Artifact Registry";
    case "mcr":
      return "Microsoft Container Registry";
    case "custom":
      return "Custom Registry";
  }
}

export function getRegistryCredentialProviderHelp(
  provider: RegistryCredentialProvider,
) {
  switch (provider) {
    case "ghcr":
      return {
        usernameLabel: null,
        secretLabel: "GitHub Access Token",
        description:
          "Use a GitHub Personal Access Token with the read:packages scope.",
      };
    case "docker-hub":
      return {
        usernameLabel: "Username",
        secretLabel: "Personal Access Token",
        description:
          "Use your Docker ID and a Docker Hub personal access token.",
      };
    case "gitlab":
      return {
        usernameLabel: "Username",
        secretLabel: "Personal Access Token",
        description:
          "Use your GitLab username and a token with read_registry scope.",
      };
    case "quay":
      return {
        usernameLabel: "Robot Username",
        secretLabel: "Robot Token",
        description:
          "Use the Quay robot username in namespace+robotname format and its generated token.",
      };
    case "aws-ecr-public":
      return {
        usernameLabel: "Username",
        secretLabel: "Authentication Token",
        description:
          "Use AWS as the username and an authentication token from aws ecr get-login-password.",
      };
    case "gcp-artifact-registry":
      return {
        usernameLabel: "Username",
        secretLabel: "Service Account JSON Key",
        description:
          "Use _json_key as the username and your service account JSON key contents as the secret.",
      };
    case "mcr":
      return {
        usernameLabel: "Username",
        secretLabel: "Password or Token",
        description:
          "Many MCR images are public, but private images can use standard Docker authentication.",
      };
    case "custom":
      return {
        usernameLabel: "Username",
        secretLabel: "Password or Token",
        description:
          "Use any registry credentials that work with standard Docker authentication.",
      };
  }
}

export function projectServiceDeploymentConfig(
  service: ServiceDeploymentFieldSelection,
): ServiceDeploymentConfig {
  const config: ServiceDeploymentConfigProjection = {
    version: 2,
    name: service.name,
    source: service.source,
    preDeployCommand: service.preDeployCommand,
    startCommand: service.startCommand,
    healthcheck: service.healthcheck,
    restartPolicy: service.restartPolicy,
    privateDns: service.privateDns,
    env: service.env ?? {},
    mounts: service.mounts ?? [],
    variableGroupAttachments: service.variableGroupAttachments ?? [],
  };
  if (service.maxRetries !== undefined) config.maxRetries = service.maxRetries;
  if (service.cron !== undefined) config.cron = service.cron;
  if (service.replicas !== undefined) config.replicas = service.replicas;
  if (service.cpuLimit !== undefined) config.cpuLimit = service.cpuLimit;
  if (service.memLimit !== undefined) config.memLimit = service.memLimit;
  if (service.routes !== undefined) config.routes = service.routes;
  if (service.managedHostname !== undefined) {
    config.managedHostname = service.managedHostname;
  }
  if (service.build !== undefined) config.build = service.build;
  return decodeStrict(serviceDeploymentConfigSchema, config);
}
