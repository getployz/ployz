import {
  type ContainerMountPath,
  type ImageReference,
  type InstallArtifactVersion,
  type MachineName,
  type RouteHostname,
  type RoutePort,
  type VolumeName,
} from "./generated.ts";

export function machineName(value: string): MachineName {
  return subjectToken(value, "machine name") as MachineName;
}

export function imageReference(value: string): ImageReference {
  if (value.trim() === "" || /[\s\p{C}]/u.test(value)) {
    throw new RangeError("image reference must be non-empty and contain no whitespace");
  }

  return value as ImageReference;
}

export function volumeName(value: string): VolumeName {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) {
    throw new RangeError("volume name must contain only ASCII letters, digits, '_' or '-'");
  }

  return value as VolumeName;
}

export function containerMountPath(value: string): ContainerMountPath {
  if (!value.startsWith("/") || value.includes("\0")) {
    throw new RangeError("container mount path must be an absolute path without NUL");
  }

  return value as ContainerMountPath;
}

export function installArtifactVersion(value: string): InstallArtifactVersion {
  if (value === "" || /[^\x21-\x7E]/.test(value)) {
    throw new RangeError("install artifact version must contain only visible ASCII");
  }

  return value as InstallArtifactVersion;
}

export function routeHostname(value: string): RouteHostname {
  if (
    value === "" ||
    value.split(".").some((label) => label === "" || label.startsWith("-") || label.endsWith("-")) ||
    !/^[A-Za-z0-9.-]+$/.test(value)
  ) {
    throw new RangeError("route hostname is invalid");
  }

  return value.toLowerCase() as RouteHostname;
}

export function routePort(value: number): RoutePort {
  if (!Number.isSafeInteger(value) || value < 1 || value > 65_535) {
    throw new RangeError("route port must be an integer from 1 to 65535");
  }

  return value as RoutePort;
}

function subjectToken(value: string, label: string): string {
  if (value === "" || !/^[A-Za-z0-9_-]+$/.test(value)) {
    throw new RangeError(`${label} must contain only ASCII letters, numbers, underscores, or dashes`);
  }

  return value;
}
