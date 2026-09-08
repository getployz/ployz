import { compareResourceSettings, type VolumeConfig, type VariableGroupConfig, compareServiceSettings, type ChangeKind, type ServiceSettingChange } from "@ployz/sdk/config";
import { asBoolean, asFiniteNumber, asString, asRecord } from "#/lib/json";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";

export type ServiceDeploymentDiffKind = ChangeKind;
export type ServiceDeploymentDiffPath = ServiceSettingChange["path"];
type ManagedHostnameValue = ServiceDeploymentConfig["managedHostname"];

export const SERVICE_DEPLOYMENT_DIFF_PATHS = {
  source: "source",
  name: "name",
  sourceRepository: "source.repository",
  sourceBranch: "source.branch",
  sourceRootDir: "source.rootDir",
  sourceAutoDeploy: "source.autoDeploy",
  sourceWaitForCi: "source.waitForCi",
  sourceImage: "source.image",
  sourceAutoUpdate: "source.autoUpdate",
  sourceCredentials: "source.credentials",
  preDeployCommand: "preDeployCommand",
  startCommand: "startCommand",
  healthcheck: "healthcheck",
  restartPolicy: "restartPolicy",
  maxRetries: "maxRetries",
  cron: "cron",
  replicas: "replicas",
  cpuLimit: "cpuLimit",
  memLimit: "memLimit",
  privateDns: "privateDns",
  routes: "routes",
  managedHostname: "managedHostname",
  build: "build",
} as const;


const labels = new Map(Object.entries({
  name: "Name", source: "Source", "source.repository": "Repository",
  "source.branch": "Branch", "source.rootDir": "Root directory",
  "source.autoDeploy": "Auto-deploy", "source.waitForCi": "Deploy after CI passes",
  "source.image": "Container image", "source.autoUpdate": "Auto-update",
  "source.credentials": "Credentials", preDeployCommand: "Pre-deploy command",
  startCommand: "Start command", healthcheck: "Healthcheck", restartPolicy: "Restart policy",
  maxRetries: "Max retries", cron: "Cron schedule", replicas: "Replicas",
  cpuLimit: "CPU limit", memLimit: "Memory limit", privateDns: "Private DNS",
  managedHostname: "Managed domain", build: "Build",
  variableGroupAttachments: "Variable Group attachments (in precedence order)",
}));

function displaySetting(path: string, value: ServiceSettingChange["before"]): string {
  if (value == null) return "";
  if (path === "variableGroupAttachments") {
    return Array.isArray(value) && value.length
      ? value.map((attachment) => asString(asRecord(attachment)?.["variableGroupId"]) ?? "Unknown group").join(" → ")
      : "None";
  }
  const record = asRecord(value);
  if (path.startsWith("env.")) return record?.["kind"] === "secret" ? "Secret value" : asString(record?.["value"]) ?? "";
  if (path.startsWith("mounts.")) return asString(record?.["mountPath"]) ?? "";
  if (path.startsWith("routes.")) return `${asString(record?.["hostname"])}:${asFiniteNumber(record?.["targetPort"])}`;
  switch (path) {
    case "source": return asString(record?.["type"]) ?? "";
    case "source.branch": return asString(record?.["name"] ?? record?.["previousName"]) ?? "Disconnected";
    case "source.autoUpdate": return record?.["type"] === "off" ? "Off" : asString(record?.["tag"]) ?? "";
    case "source.credentials": return record?.["type"] === "none" ? "None" : "Configured";
    case "healthcheck": return record?.["type"] === "none" ? "Disabled" : `${asString(record?.["path"])} (${asFiniteNumber(record?.["timeoutSeconds"])}s timeout)`;
    case "restartPolicy": return ({ always: "Always", "on-failure": "On failure", no: "No", "unless-stopped": "Unless stopped" })[asString(value) ?? ""] ?? "";
    case "managedHostname": return `${asString(record?.["prefix"])} (port ${asFiniteNumber(record?.["targetPort"]) ?? "PORT"})`;
    case "build": return record?.["builder"] === "dockerfile" ? (record["dockerfilePath"] ? `Dockerfile (${asString(record["dockerfilePath"])})` : "Dockerfile") : "Auto-detect";
    case "cpuLimit": return `${asFiniteNumber(value)} vCPU`;
    case "memLimit": return `${asFiniteNumber(value)} GB`;
    default: {
      const bool = asBoolean(value);
      return bool !== null ? (bool ? "Enabled" : "Disabled") : asString(value) ?? String(asFiniteNumber(value) ?? "");
    }
  }
}

/**
 * Detects cluster-domain drift for a service's managed auto hostname: the URL is
 * derived (`{prefix}.{autoDomain}`), so a cluster-domain change is not a config
 * diff — it's runtime-derived. Returns a synthetic staged-change row when the
 * service is serving its managed hostname under a domain other than the current
 * one, or null otherwise. Keyed on the current prefix; a prefix change already
 * diffs via the `managedHostname` config field.
 */
export function getManagedHostnameDriftRow(input: {
  serviceId: string;
  managedHostname: ManagedHostnameValue;
  autoDomain: string | null;
  boundHostnames: string[];
}): ServiceDeploymentDiffRow | null {
  const { managedHostname, autoDomain } = input;
  if (!managedHostname || !autoDomain) {
    return null;
  }
  const expected = `${managedHostname.prefix}.${autoDomain}`;
  const stale = input.boundHostnames.find(
    (hostname) =>
      hostname.startsWith(`${managedHostname.prefix}.`) && hostname !== expected,
  );
  if (!stale) {
    return null;
  }
  return {
    changeKey: `${input.serviceId}:managedHostname.drift`,
    label: "Public URL",
    kind: "update",
    path: "managedHostname.drift",
    currentValue: stale,
    newValue: expected,
    canDiscard: false,
  };
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
  derivedFrom?: {
    kind: "variable_group";
    resourceId: string;
    resourceName: string;
  };
};

export type ServiceDeploymentDiffRow = Omit<DiffRow, "path"> & {
  path: ServiceDeploymentDiffPath;
};

/** Changes the node owns — derived (inherited) rows don't count as its changes. */
export function countOwnedRows(rows: Pick<DiffRow, "derivedFrom">[]): number {
  return rows.filter((row) => !row.derivedFrom).length;
}

export function getDiffKind<TBaseline, TCurrent>(
  baselineValue: TBaseline,
  currentValue: TCurrent,
): ServiceDeploymentDiffKind {
  if (baselineValue == null) {
    return "add";
  }

  if (currentValue == null) {
    return "remove";
  }

  return "update";
}

export function getServiceDeploymentDiffRows(input: {
  serviceId: string;
  current: ServiceDeploymentConfig;
  baseline: ServiceDeploymentConfig | null;
}): ServiceDeploymentDiffRow[] {
  return compareServiceSettings(input.current, input.baseline).map((change) => ({
    changeKey: `${input.serviceId}:${change.path}`,
    path: change.path,
    ...presentSettingChange("service", change.path, change.before, change.after),
    kind: change.kind,
    canDiscard: change.canRestore,
    derivedFrom: change.derivedFrom,
  }));
}

/** Resource labels and redacted value formatting; Rust supplies change policy. */
export function getResourceDeploymentDiffRows(nodeType: "volume" | "variable_group", input: {
  nodeId: string;
  current: VolumeConfig | VariableGroupConfig;
  baseline: VolumeConfig | VariableGroupConfig | null;
}): DiffRow[] {
  return compareResourceSettings(nodeType, input.current, input.baseline).map((change) => ({
    changeKey: `${input.nodeId}:${change.path}`,
    path: change.path,
    ...presentSettingChange(nodeType, change.path, change.before, change.after),
    kind: change.kind,
    canDiscard: change.canRestore,
  }));
}

export function presentSettingChange(nodeType: "service" | "volume" | "variable_group", path: string,
  before: ServiceSettingChange["before"], after: ServiceSettingChange["after"]) {
  if (nodeType !== "service") {
    const display = (value: ServiceSettingChange["before"]) =>
      asRecord(value)?.["kind"] === "secret" ? "Secret value" : asString(value) ?? "";
    return {
      label: path === "node" ? (nodeType === "volume" ? "Volume" : "Variable Group")
        : path === "name" ? "Name" : `Variable ${path.slice(10)}`,
      currentValue: display(before), newValue: display(after),
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
