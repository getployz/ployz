import { Schema } from "effect";

/** The registries whose credentials the form knows how to ask for. */
type RegistryCredentialProvider =
  | "docker-hub" | "ghcr" | "gitlab" | "quay" | "aws-ecr-public" | "gcp-artifact-registry" | "mcr" | "custom";

export const registryCredentialUsernameSchema = Schema.Trim.check(
  Schema.isNonEmpty({ message: "Username is required" }),
  Schema.isMaxLength(255, {
    message: "Username must be 255 characters or fewer",
  }),
);
export const registryCredentialSecretSchema = Schema.String.check(
  Schema.isNonEmpty({ message: "Secret is required" }),
  Schema.isMaxLength(32_768, {
    message: "Secret must be 32768 characters or fewer",
  }),
);

const dockerHubHosts = new Set(["docker.io", "index.docker.io"]);

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

