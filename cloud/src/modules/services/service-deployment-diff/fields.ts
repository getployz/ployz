import { asBoolean, asFiniteNumber, asString } from "#/lib/json";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import {
  areDeepEqual,
  getValueAtPath,
  type WidePath,
  type WidePathValue,
} from "#/utils/schema-path";

/**
 * Deployment diffs are defined by the `fieldDefinitions` registry below.
 *
 * To add a new diff:
 * 1. Add one `defineField({ path, label, ... })` entry for the
 *    `ServiceDeploymentConfig` field you want to track.
 * 2. Prefer pointing `path` at the real config value and let the generic
 *    compare/revert logic handle equality and discard behavior.
 * 3. Add `getDisplayValue` only when the raw value needs product-specific
 *    presentation, such as discriminated unions or boolean labels.
 * 4. Add `shouldInclude` only when a field should be hidden for default or
 *    source-specific states instead of purely diffing by value.
 */
export type ServiceDeploymentDiffKind = "add" | "update" | "remove";

type ServiceDeploymentFieldPath = WidePath<ServiceDeploymentConfig>;

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
} as const satisfies Record<string, ServiceDeploymentFieldPath>;

type BranchValue = Extract<
  ServiceDeploymentConfig["source"],
  { type: "git" }
>["branch"];
type AutoUpdateValue = Extract<
  ServiceDeploymentConfig["source"],
  { type: "image" }
>["autoUpdate"];
type CredentialsValue = Extract<
  ServiceDeploymentConfig["source"],
  { type: "image" }
>["credentials"];
type HealthcheckValue = ServiceDeploymentConfig["healthcheck"];
type RestartPolicyValue = ServiceDeploymentConfig["restartPolicy"];
type ManagedHostnameValue = ServiceDeploymentConfig["managedHostname"];
type BuildValue = ServiceDeploymentConfig["build"];
type CpuLimitValue = ServiceDeploymentConfig["cpuLimit"];
type MemLimitValue = ServiceDeploymentConfig["memLimit"];
type EnvValue = ServiceDeploymentConfig["env"][string];

type FieldValueOf<TPath extends ServiceDeploymentFieldPath> = WidePathValue<
  ServiceDeploymentConfig,
  TPath
>;

const DEFAULT_BUILD_VALUE: BuildValue = {
  builder: "auto",
  dockerfilePath: null,
  watchPaths: [],
};

type DiffFieldValue =
  | FieldValueOf<ServiceDeploymentFieldPath>
  | EnvValue
  | undefined;

type ErasedFieldDefinition = {
  path: ServiceDeploymentFieldPath;
  label: string;
  canDiscard?: boolean;
  getDisplayValue?: (value: DiffFieldValue) => string | null;
  shouldInclude?: (input: {
    current: ServiceDeploymentConfig;
    baseline: ServiceDeploymentConfig | null;
    currentValue: DiffFieldValue;
    baselineValue: DiffFieldValue;
  }) => boolean;
};

function defineField<TPath extends ServiceDeploymentFieldPath>(definition: {
  path: TPath;
  label: string;
  canDiscard?: boolean;
  getDisplayValue?: (value: FieldValueOf<TPath>) => string | null;
  shouldInclude?: (input: {
    current: ServiceDeploymentConfig;
    baseline: ServiceDeploymentConfig | null;
    currentValue: FieldValueOf<TPath>;
    baselineValue: FieldValueOf<TPath> | null | undefined;
  }) => boolean;
}): ErasedFieldDefinition {
  const getDisplayValue = definition.getDisplayValue;
  const shouldInclude = definition.shouldInclude;
  // SAFETY: ErasedFieldDefinition widens TPath; callbacks still receive the value at that path.
  return {
    path: definition.path,
    label: definition.label,
    canDiscard: definition.canDiscard,
    getDisplayValue: getDisplayValue
      ? (value) =>
          getDisplayValue(value as typeof value & FieldValueOf<TPath>)
      : undefined,
    shouldInclude: shouldInclude
      ? (input) =>
          shouldInclude({
            current: input.current,
            baseline: input.baseline,
            currentValue: input.currentValue as typeof input.currentValue &
              FieldValueOf<TPath>,
            baselineValue: input.baselineValue as typeof input.baselineValue &
              FieldValueOf<TPath>,
          })
      : undefined,
  };
}

function getDisplayValue(value: DiffFieldValue) {
  if (value == null) {
    return null;
  }

  const bool = asBoolean(value);
  if (bool !== null) return bool ? "Enabled" : "Disabled";
  const text = asString(value);
  if (text !== null) return text;
  const num = asFiniteNumber(value);
  if (num !== null) return String(num);
  return null;
}

function getBranchDisplayValue(value: BranchValue | null | undefined) {
  if (value == null) {
    return null;
  }

  if (value.type === "connected") {
    return value.name;
  }

  return value.previousName ?? "Disconnected";
}

function getAutoUpdateDisplayValue(value: AutoUpdateValue | null | undefined) {
  if (value == null) {
    return null;
  }

  if (value.type === "off") {
    return "Off";
  }

  return value.tag;
}

function getCredentialsDisplayValue(value: CredentialsValue | null | undefined) {
  if (value == null) {
    return null;
  }

  if (value.type === "none") {
    return "None";
  }

  return "Configured";
}

function getHealthcheckDisplayValue(value: HealthcheckValue | null | undefined) {
  if (value == null) {
    return null;
  }

  if (value.type === "none") {
    return "Disabled";
  }

  return `${value.path} (${value.timeoutSeconds}s timeout)`;
}

function getRestartPolicyDisplayValue(
  value: RestartPolicyValue | null | undefined,
) {
  if (value == null) {
    return null;
  }

  switch (value) {
    case "always":
      return "Always";
    case "on-failure":
      return "On failure";
    case "no":
      return "No";
    case "unless-stopped":
      return "Unless stopped";
  }
}

function getManagedHostnameDisplayValue(value: ManagedHostnameValue) {
  if (value == null) {
    return null;
  }

  const port = value.targetPort == null ? "PORT" : String(value.targetPort);
  return `${value.prefix} (port ${port})`;
}

function getBuildDisplayValue(value: BuildValue) {
  if (value.builder === "dockerfile") {
    return value.dockerfilePath
      ? `Dockerfile (${value.dockerfilePath})`
      : "Dockerfile";
  }

  return "Auto-detect";
}

function getCpuLimitDisplayValue(value: CpuLimitValue) {
  return value == null ? null : `${value} vCPU`;
}

function getMemLimitDisplayValue(value: MemLimitValue) {
  return value == null ? null : `${value} GB`;
}

function areValuesEqual<TLeft, TRight>(left: TLeft, right: TRight) {
  return areDeepEqual(left, right);
}

/** Scalar with a non-null default: show when it drifts from the deployed
 * baseline, or (pre-first-deploy) from the config default. */
function includeScalarWithDefault<T extends DiffFieldValue>(defaultValue: T) {
  return ({
    baseline,
    currentValue,
    baselineValue,
  }: {
    baseline: ServiceDeploymentConfig | null;
    currentValue: DiffFieldValue;
    baselineValue: DiffFieldValue;
  }) =>
    baseline
      ? !areValuesEqual(currentValue, baselineValue)
      : !areValuesEqual(currentValue, defaultValue);
}

/** Nullable field: show when either side is set. */
function includeWhenEitherSet({
  currentValue,
  baselineValue,
}: {
  currentValue: DiffFieldValue;
  baselineValue: DiffFieldValue;
}) {
  return currentValue != null || baselineValue != null;
}

const fieldDefinitions = [
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.sourceRepository,
    label: "Repository",
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.sourceBranch,
    label: "Branch",
    getDisplayValue: getBranchDisplayValue,
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.sourceRootDir,
    label: "Root directory",
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.sourceAutoDeploy,
    label: "Auto-deploy",
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.sourceWaitForCi,
    label: "Deploy after CI passes",
    shouldInclude: ({ current, baseline, currentValue, baselineValue }) => {
      if (current.source.type === "git" && baseline?.source.type === "git") {
        return !areValuesEqual(currentValue, baselineValue);
      }

      if (current.source.type === "git") {
        return current.source.waitForCi;
      }

      return baseline?.source.type === "git"
        ? baseline.source.waitForCi
        : false;
    },
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.sourceImage,
    label: "Container image",
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.sourceAutoUpdate,
    label: "Auto-update",
    getDisplayValue: getAutoUpdateDisplayValue,
    shouldInclude: ({ current, baseline, currentValue, baselineValue }) => {
      if (
        current.source.type === "image" &&
        baseline?.source.type === "image"
      ) {
        return !areValuesEqual(currentValue, baselineValue);
      }

      if (current.source.type === "image") {
        return current.source.autoUpdate.type !== "off";
      }

      return baseline?.source.type === "image"
        ? baseline.source.autoUpdate.type !== "off"
        : false;
    },
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.sourceCredentials,
    label: "Credentials",
    getDisplayValue: getCredentialsDisplayValue,
    shouldInclude: ({ current, baseline, currentValue, baselineValue }) => {
      if (
        current.source.type === "image" &&
        baseline?.source.type === "image"
      ) {
        return !areValuesEqual(currentValue, baselineValue);
      }

      if (current.source.type === "image") {
        return current.source.credentials.type !== "none";
      }

      return baseline?.source.type === "image"
        ? baseline.source.credentials.type !== "none"
        : false;
    },
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.preDeployCommand,
    label: "Pre-deploy command",
    shouldInclude: ({ currentValue, baselineValue }) =>
      currentValue != null || baselineValue != null,
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.startCommand,
    label: "Start command",
    shouldInclude: ({ currentValue, baselineValue }) =>
      currentValue != null || baselineValue != null,
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.healthcheck,
    label: "Healthcheck",
    getDisplayValue: getHealthcheckDisplayValue,
    shouldInclude: ({ current, baseline, currentValue, baselineValue }) => {
      if (baseline) {
        return !areValuesEqual(currentValue, baselineValue);
      }

      return current.healthcheck.type !== "none";
    },
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.restartPolicy,
    label: "Restart policy",
    getDisplayValue: getRestartPolicyDisplayValue,
    shouldInclude: ({ current, baseline, currentValue, baselineValue }) => {
      if (baseline) {
        return !areValuesEqual(currentValue, baselineValue);
      }

      return current.restartPolicy !== "unless-stopped";
    },
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.maxRetries,
    label: "Max retries",
    shouldInclude: includeScalarWithDefault(10),
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.cron,
    label: "Cron schedule",
    shouldInclude: includeWhenEitherSet,
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.replicas,
    label: "Replicas",
    shouldInclude: includeScalarWithDefault(1),
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.cpuLimit,
    label: "CPU limit",
    getDisplayValue: getCpuLimitDisplayValue,
    shouldInclude: includeWhenEitherSet,
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.memLimit,
    label: "Memory limit",
    getDisplayValue: getMemLimitDisplayValue,
    shouldInclude: includeWhenEitherSet,
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.privateDns,
    label: "Private DNS",
    shouldInclude: includeWhenEitherSet,
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.managedHostname,
    label: "Managed domain",
    getDisplayValue: getManagedHostnameDisplayValue,
    shouldInclude: includeWhenEitherSet,
  }),
  defineField({
    path: SERVICE_DEPLOYMENT_DIFF_PATHS.build,
    label: "Build",
    getDisplayValue: getBuildDisplayValue,
    shouldInclude: ({ baseline, currentValue, baselineValue }) =>
      baseline
        ? !areValuesEqual(currentValue, baselineValue)
        : !areValuesEqual(currentValue, DEFAULT_BUILD_VALUE),
  }),
] as const;

export type ServiceDeploymentDiffPath =
  | "source"
  | "routes"
  | (typeof fieldDefinitions)[number]["path"]
  | `routes.${string}`
  | `env.${string}`
  | `mounts.${string}`
  // Synthetic, runtime-derived: the managed public URL was minted under an older
  // cluster domain and will regenerate under the current one on redeploy.
  | "managedHostname.drift";

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

export function getPathValue(
  config: ServiceDeploymentConfig | null,
  path: ServiceDeploymentDiffPath,
): DiffFieldValue {
  if (!config) {
    return undefined;
  }

  if (path.startsWith("env.")) {
    const key = path.slice("env.".length);
    return config.env[key];
  }

  // SAFETY: remaining paths are WidePath keys; getValueAtPath cannot prove DeepPath after env. handling.
  return getValueAtPath(config as never, path as never);
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
}) {
  const rows: ServiceDeploymentDiffRow[] = [];

  if (
    input.baseline &&
    input.current.source.type !== input.baseline.source.type
  ) {
    rows.push({
      changeKey: `${input.serviceId}:source`,
      label: "Source",
      kind: "update",
      path: SERVICE_DEPLOYMENT_DIFF_PATHS.source,
      currentValue: input.baseline.source.type,
      newValue: input.current.source.type,
      canDiscard: true,
    });
  }

  const currentGitSource =
    input.current.source.type === "git" ? input.current.source : null;
  const baselineGitSource =
    input.baseline?.source.type === "git" ? input.baseline.source : null;
  const repositoryChanged =
    currentGitSource != null &&
    baselineGitSource != null &&
    (currentGitSource.repository !== baselineGitSource.repository ||
      currentGitSource.repositoryId !== baselineGitSource.repositoryId ||
      currentGitSource.installationId !== baselineGitSource.installationId);
  if (repositoryChanged) {
    rows.push({
      changeKey: `${input.serviceId}:source.repository`,
      label: "Repository",
      kind: "update",
      path: SERVICE_DEPLOYMENT_DIFF_PATHS.sourceRepository,
      currentValue: baselineGitSource?.repository ?? "",
      newValue: currentGitSource?.repository ?? "",
      canDiscard: true,
    });
  }

  for (const field of fieldDefinitions) {
    if (
      repositoryChanged &&
      field.path === SERVICE_DEPLOYMENT_DIFF_PATHS.sourceRepository
    ) {
      continue;
    }
    if (
      input.baseline &&
      input.current.source.type !== input.baseline.source.type &&
      field.path.startsWith("source.")
    ) {
      continue;
    }
    const baselineValue = getPathValue(input.baseline, field.path);
    const currentValue = getPathValue(input.current, field.path);

    const shouldInclude =
      field.shouldInclude?.({
        current: input.current,
        baseline: input.baseline,
        currentValue,
        baselineValue,
      }) ?? currentValue !== baselineValue;

    if (!shouldInclude || areValuesEqual(baselineValue, currentValue)) {
      continue;
    }

    rows.push({
      changeKey: `${input.serviceId}:${field.path}`,
      label: field.label,
      kind: getDiffKind(baselineValue, currentValue),
      path: field.path,
      currentValue:
        field.getDisplayValue?.(baselineValue) ??
        getDisplayValue(baselineValue) ??
        "",
      newValue:
        field.getDisplayValue?.(currentValue) ??
        getDisplayValue(currentValue) ??
        "",
      canDiscard: input.baseline != null && field.canDiscard !== false,
    });
  }

  rows.push(...getServiceDeploymentEnvDiffRows(input));
  rows.push(...getServiceDeploymentMountDiffRows(input));
  rows.push(...getServiceDeploymentRouteDiffRows(input));

  return rows;
}

function getServiceDeploymentRouteDiffRows(input: {
  serviceId: string;
  current: ServiceDeploymentConfig;
  baseline: ServiceDeploymentConfig | null;
}) {
  const baselineRoutes = new Map(
    (input.baseline?.routes ?? []).map((route) => [route.id, route]),
  );
  const currentRoutes = new Map(
    input.current.routes.map((route) => [route.id, route]),
  );
  const ids = [
    ...new Set([...baselineRoutes.keys(), ...currentRoutes.keys()]),
  ].sort();

  return ids.flatMap<ServiceDeploymentDiffRow>((id) => {
    const baselineRoute = baselineRoutes.get(id);
    const currentRoute = currentRoutes.get(id);
    if (areValuesEqual(baselineRoute, currentRoute)) {
      return [];
    }
    const display = (route: typeof currentRoute) =>
      route ? `${route.hostname}:${route.targetPort}` : "";

    return [
      {
        changeKey: `${input.serviceId}:routes.${id}`,
        label: "Public route",
        kind: getDiffKind(baselineRoute, currentRoute),
        path: `routes.${id}`,
        currentValue: display(baselineRoute),
        newValue: display(currentRoute),
        canDiscard: input.baseline != null,
      },
    ];
  });
}

function getServiceDeploymentMountDiffRows(input: {
  serviceId: string;
  current: ServiceDeploymentConfig;
  baseline: ServiceDeploymentConfig | null;
}) {
  const baselineMounts = new Map(
    (input.baseline?.mounts ?? []).map((mount) => [mount.volumeResourceId, mount]),
  );
  const currentMounts = new Map(
    input.current.mounts.map((mount) => [mount.volumeResourceId, mount]),
  );
  const ids = [...new Set([...baselineMounts.keys(), ...currentMounts.keys()])].sort();
  const rows: ServiceDeploymentDiffRow[] = [];

  for (const id of ids) {
    const baselineMount = baselineMounts.get(id);
    const currentMount = currentMounts.get(id);
    // A volume rename alone is not a service change — only the mount path (the
    // runtime container config) matters here (R16).
    if (baselineMount?.mountPath === currentMount?.mountPath) {
      continue;
    }

    const mount = currentMount ?? baselineMount;
    rows.push({
      changeKey: `${input.serviceId}:mounts.${id}`,
      label: `Volume mount ${mount?.volumeName ?? id}`,
      kind: getDiffKind(baselineMount, currentMount),
      path: `mounts.${id}`,
      currentValue: baselineMount?.mountPath ?? "",
      newValue: currentMount?.mountPath ?? "",
      // Mounts are owned service changes but managed from the volume drawer, so
      // they're not independently row-discardable (a volume-delete cascade is
      // discarded at the volume node).
      canDiscard: false,
    });
  }

  return rows;
}

function getEnvDisplayValue(value: EnvValue | undefined) {
  if (!value) {
    return "";
  }

  if (value.kind === "secret") {
    return "Secret value";
  }

  return value.value;
}

function getComparableEnvValue(value: EnvValue | undefined) {
  if (!value) {
    return undefined;
  }

  if (value.kind === "secret") {
    return {
      kind: value.kind,
      variableId: value.variableId,
      fingerprint: value.fingerprint,
    };
  }

  // Compare the template/display string only; `parts` is a deploy-time resolution
  // detail and the editing config never carries it.
  return { kind: value.kind, value: value.value };
}

function getDerivedEnvSource(
  currentValue: EnvValue | undefined,
  baselineValue: EnvValue | undefined,
) {
  const source =
    currentValue?.source ?? (!currentValue ? baselineValue?.source : undefined);

  if (!source || source.kind !== "variable_group") {
    return undefined;
  }

  return {
    kind: source.kind,
    resourceId: source.resourceId,
    resourceName: source.resourceName,
  };
}

function getServiceDeploymentEnvDiffRows(input: {
  serviceId: string;
  current: ServiceDeploymentConfig;
  baseline: ServiceDeploymentConfig | null;
}) {
  const baselineEnv = input.baseline?.env ?? {};
  const currentEnv = input.current.env ?? {};
  const keys = [...new Set([
    ...Object.keys(baselineEnv),
    ...Object.keys(currentEnv),
  ])].sort();
  const rows: ServiceDeploymentDiffRow[] = [];

  for (const key of keys) {
    const baselineValue = baselineEnv[key];
    const currentValue = currentEnv[key];

    if (
      areValuesEqual(
        getComparableEnvValue(baselineValue),
        getComparableEnvValue(currentValue),
      )
    ) {
      continue;
    }

    rows.push({
      changeKey: `${input.serviceId}:env.${key}`,
      label: `Environment variable ${key}`,
      kind: getDiffKind(baselineValue, currentValue),
      path: `env.${key}`,
      currentValue: getEnvDisplayValue(baselineValue),
      newValue: getEnvDisplayValue(currentValue),
      canDiscard: false,
      derivedFrom: getDerivedEnvSource(currentValue, baselineValue),
    });
  }

  return rows;
}
