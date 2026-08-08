import {
  type ContainerId,
  type ContainerMountPath,
  type ImageReference,
  type InstallArtifactVersion,
  type MachineName,
  type RouteHostname,
  type RoutePort,
  type StopGracePeriod,
  type VolumeName,
} from "./generated.ts";

export function machineName(value: string): MachineName {
  return subjectToken(value, "machine name") as MachineName;
}

export function containerId(value: string): ContainerId {
  return subjectToken(value, "container id") as ContainerId;
}

export function imageReference(value: string): ImageReference {
  if (value.trim() === "") {
    throw new RangeError("image reference must not be empty");
  }
  if (/[\s\p{C}]/u.test(value)) {
    throw new RangeError("image reference must not contain whitespace or control characters");
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
  return visibleAscii(value, "install artifact version") as InstallArtifactVersion;
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
  return positiveU16(value, "route port") as RoutePort;
}

export function stopGracePeriod(value: number): StopGracePeriod {
  if (!Number.isSafeInteger(value) || value < 0 || value > 0xffff_ffff) {
    throw new RangeError("stop grace period must be an integer number of seconds within u32 range");
  }

  return value as StopGracePeriod;
}

function subjectToken(value: string, label: string): string {
  if (value === "") {
    throw new RangeError(`${label} must not be empty`);
  }
  if (!/^[A-Za-z0-9_-]+$/.test(value)) {
    throw new RangeError(`${label} must contain only ASCII letters, numbers, underscores, or dashes`);
  }

  return value;
}

function visibleAscii(value: string, label: string): string {
  if (value === "") {
    throw new RangeError(`${label} must not be empty`);
  }
  if (/[^\x21-\x7E]/.test(value)) {
    throw new RangeError(`${label} must contain only visible ASCII without whitespace`);
  }

  return value;
}

function positiveU16(value: number, label: string): number {
  if (!Number.isSafeInteger(value) || value < 1 || value > 65_535) {
    throw new RangeError(`${label} must be an integer from 1 to 65535`);
  }

  return value;
}
