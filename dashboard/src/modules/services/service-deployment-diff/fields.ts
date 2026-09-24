import { compareServiceSettings, type ChangeKind, type ServiceSettingChange } from "@ployz/sdk/config";
import { asBoolean, asFiniteNumber, asString, asRecord } from "#/lib/json";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";

export type ServiceDeploymentDiffKind = ChangeKind;
export type ServiceDeploymentDiffPath = ServiceSettingChange["path"];

export const SERVICE_DEPLOYMENT_DIFF_PATHS = {
  source: "source",
  sourceRepository: "source.repository",
  sourceBranch: "source.branch",
  sourceRootDir: "source.rootDir",
  sourceImage: "source.image",
  sourceCredentials: "source.credentials",
  preDeployCommand: "preDeployCommand",
  startCommand: "startCommand",
  healthcheck: "healthcheck",
  healthcheckPath: "healthcheck.path",
  healthcheckTimeout: "healthcheck.timeoutSeconds",
  restartPolicy: "restartPolicy",
  maxRetries: "maxRetries",
  replicas: "replicas",
  cpuLimit: "cpuLimit",
  memLimit: "memLimit",
  privateDns: "privateDns",
  routes: "routes",
  managedHostnames: "managedHostnames",
  buildBuilder: "build.builder",
  buildCommand: "build.command",
  buildDockerfilePath: "build.dockerfilePath",
} as const;


const labels = new Map(Object.entries({
  source: "Source", "source.repository": "Repository",
  "source.branch": "Branch", "source.rootDir": "Root directory",
  "source.image": "Container image",
  "source.credentials": "Credentials", preDeployCommand: "Pre-deploy command",
  startCommand: "Start command", healthcheck: "Healthcheck", "healthcheck.path": "Healthcheck path", "healthcheck.timeoutSeconds": "Healthcheck timeout", restartPolicy: "Restart policy",
  maxRetries: "Max retries", replicas: "Replicas",
  cpuLimit: "CPU limit", memLimit: "Memory limit", privateDns: "Private DNS",
  "build.command": "Build command",
  managedHostnames: "Managed domains", "build.builder": "Builder", "build.dockerfilePath": "Dockerfile path",
}));

function displaySetting(path: string, value: ServiceSettingChange["before"]): string {
  if (value == null) return "";
  const record = asRecord(value);
  if (path.startsWith("env.")) return record?.["kind"] === "secret" ? "Secret value" : asString(record?.["value"]) ?? "";
  if (path.startsWith("mounts.")) return asString(record?.["mountPath"]) ?? "";
  if (path.startsWith("routes.")) return `${asString(record?.["hostname"])}:${asFiniteNumber(record?.["targetPort"]) ?? "PORT"}`;
  switch (path) {
    case "source": return asString(record?.["type"]) ?? "";
    case "source.branch": return asString(record?.["name"] ?? record?.["previousName"]) ?? "Disconnected";
    case "source.credentials": return record?.["type"] === "none" ? "None" : "Configured";
    case "healthcheck": return record?.["type"] === "none" ? "Disabled" : `${asString(record?.["path"])} (${asFiniteNumber(record?.["timeoutSeconds"])}s timeout)`;
    case "restartPolicy": return ({ always: "Always", "on-failure": "On Failure", no: "Never", "unless-stopped": "Unless stopped" })[asString(value) ?? ""] ?? "";
    case "managedHostnames": return Array.isArray(value) && value.length
      ? value.map((item) => { const row = asRecord(item); return `${asString(row?.["prefix"])} (port ${asFiniteNumber(row?.["targetPort"]) ?? "PORT"})`; }).join(", ")
      : "None";
    case "build.builder": return value === "dockerfile" ? "Dockerfile" : "Railpack";
    case "cpuLimit": return `${asFiniteNumber(value)} vCPU`;
    case "memLimit": return `${asFiniteNumber(value)} GB`;
    default: {
      const bool = asBoolean(value);
      return bool !== null ? (bool ? "Enabled" : "Disabled") : asString(value) ?? String(asFiniteNumber(value) ?? "");
    }
  }
}

/** A single staged change. Shared by every canvas node type; `path` is free-form. */
export type DiffRow = {
  changeKey: string;
  label: string;
  kind: ServiceDeploymentDiffKind;
  path: string;
  currentValue: string;
  newValue: string;
  canDiscard: boolean;
};

export type ServiceDeploymentDiffRow = Omit<DiffRow, "path"> & {
  path: ServiceDeploymentDiffPath;
};

export function getServiceDeploymentDiffRows(input: {
  serviceId: string;
  current: ServiceDeploymentConfig;
  baseline: ServiceDeploymentConfig | null;
}): ServiceDeploymentDiffRow[] {
  return compareServiceSettings(input.current, input.baseline)
    .map((change) => toDiffRow("service", input.serviceId, change));
}

export function toDiffRow(nodeType: "service" | "volume", nodeId: string, change: ServiceSettingChange) {
  return {
    changeKey: `${nodeId}:${change.path}`,
    path: change.path,
    ...presentSettingChange(nodeType, change.path, change.before, change.after),
    kind: change.kind,
    canDiscard: change.canRestore,
  };
}

export function presentSettingChange(nodeType: "service" | "volume", path: string,
  before: ServiceSettingChange["before"], after: ServiceSettingChange["after"]) {
  if (nodeType === "volume") {
    return {
      label: path === "node" ? "Volume" : path === "name" ? "Name" : path,
      currentValue: asString(before) ?? "", newValue: asString(after) ?? "",
    };
  }
  return {
    label: path.startsWith("env.") ? `Environment variable ${path.slice(4)}`
      : path.startsWith("routes.") ? "Public route"
      : path.startsWith("mounts.") ? `Volume mount ${asString(asRecord(after ?? before)?.["volumeName"]) ?? path.slice(7)}`
      : labels.get(path) ?? path,
    currentValue: displaySetting(path, before), newValue: displaySetting(path, after),
  };
}
