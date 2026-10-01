import { parseServiceSetting } from "@ployz/sdk/config";

/** Whether Docker could pull `image`: core's rule, the same one the Store applies when an image is written. */
export function isValidImageReference(image: string): boolean {
  try {
    parseServiceSetting("imageReference", image);
    return true;
  } catch {
    return false;
  }
}

export function imageRegistryLink(image: string): string | null {
  if (!isValidImageReference(image.trim())) return null;
  const repository = image.trim().split("@")[0]?.replace(/:[^/]+$/, "") ?? "";

  const segments = repository.split("/");
  const first = segments[0] ?? "";
  const hasRegistry = segments.length > 1 && (/[.:]/.test(first) || first === "localhost");
  const registry = hasRegistry ? first : "docker.io";
  const parts = hasRegistry ? segments.slice(1) : segments;
  const path = parts.map(encodeURIComponent).join("/");

  if (["docker.io", "index.docker.io", "registry-1.docker.io"].includes(registry)) {
    return parts.length === 1 || parts[0] === "library"
      ? `https://hub.docker.com/_/${parts.at(-1)}`
      : `https://hub.docker.com/r/${path}`;
  }
  if (registry === "quay.io") return `https://quay.io/repository/${path}`;
  return `https://${registry}/${path}`;
}
