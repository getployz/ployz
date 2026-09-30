import { Schema } from "effect";

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
  const segments = image.trim().split("/");
  const firstSegment = segments[0]?.toLowerCase() ?? "";

  // Docker reads the first part as a host only before a `/`: `nginx:1.27` is a tag on Docker Hub, not host:port.
  if (segments.length > 1 && (
    firstSegment.includes(".") ||
    firstSegment.includes(":") ||
    firstSegment === "localhost"
  )) {
    return dockerHubHosts.has(firstSegment) ? "docker.io" : firstSegment;
  }

  return "docker.io";
}

/** A registry the credentials form knows how to ask for: what it's called, and what it wants as username and secret. */
type RegistryProvider = {
  /** Whether an image's registry host is this one; the last entry takes any host. */
  matches: (host: string) => boolean;
  label: string;
  /** Null when the registry needs no username of the user's own. */
  usernameLabel: string | null;
  /** The username the registry always takes. */
  fixedUsername: string | null;
  secretLabel: string;
  /** The secret is a multi-line key file. */
  multiline?: true;
  description: string;
};

const REGISTRY_PROVIDERS: readonly RegistryProvider[] = [
  { matches: (host) => host === "ghcr.io", label: "GitHub Container Registry", usernameLabel: null, fixedUsername: null,
    secretLabel: "GitHub Access Token", description: "Use a GitHub Personal Access Token with the read:packages scope." },
  { matches: (host) => host === "registry.gitlab.com", label: "GitLab Container Registry", usernameLabel: "Username", fixedUsername: null,
    secretLabel: "Personal Access Token", description: "Use your GitLab username and a token with read_registry scope." },
  { matches: (host) => host === "quay.io", label: "Quay.io", usernameLabel: "Robot Username", fixedUsername: null, secretLabel: "Robot Token",
    description: "Use the Quay robot username in namespace+robotname format and its generated token." },
  { matches: (host) => host === "public.ecr.aws", label: "AWS ECR Public", usernameLabel: "Username", fixedUsername: "AWS",
    secretLabel: "Authentication Token", description: "Use AWS as the username and an authentication token from aws ecr get-login-password." },
  { matches: (host) => host.endsWith(".pkg.dev"), label: "Google Artifact Registry", usernameLabel: "Username", fixedUsername: "_json_key",
    secretLabel: "Service Account JSON Key", multiline: true,
    description: "Use _json_key as the username and your service account JSON key contents as the secret." },
  { matches: (host) => host === "mcr.microsoft.com", label: "Microsoft Container Registry", usernameLabel: "Username", fixedUsername: null,
    secretLabel: "Password or Token", description: "Many MCR images are public, but private images can use standard Docker authentication." },
  { matches: (host) => host === "docker.io", label: "Docker Hub", usernameLabel: "Username", fixedUsername: null,
    secretLabel: "Personal Access Token", description: "Use your Docker ID and a Docker Hub personal access token." },
  { matches: () => true, label: "Custom Registry", usernameLabel: "Username", fixedUsername: null, secretLabel: "Password or Token",
    description: "Use any registry credentials that work with standard Docker authentication." },
];

/** The registry an image pulls from, as the credentials form asks for it. */
export function registryProvider(image: string): RegistryProvider {
  const host = getRegistryHostFromImageReference(image);
  // SAFETY: the last provider matches every host.
  return REGISTRY_PROVIDERS.find((provider) => provider.matches(host)) as RegistryProvider;
}
