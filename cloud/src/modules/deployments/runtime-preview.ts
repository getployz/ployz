import type {
  DeployIntent,
  ProjectName,
  RequestedServiceSpec,
} from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { Conflict } from "#/server/public-error";
import type { JsonValue } from "#/db/tables";
import type { EnvironmentDeploymentPreview } from "#/modules/deployments/tables";
import { projectJsonValue } from "#/lib/json";
import { getVolumePhysicalName } from "#/modules/environment-design/volume-config";
import { strictParseOptions } from "#/modules/environment-design/schema";
import {
  UnsupportedDeploymentSourceError,
  type EnvironmentDeploySnapshot,
  type EnvironmentDeployVolume,
} from "#/modules/deployments/runtime-contract";

const DEFAULT_PLAN_OPTIONS = {
  force_recreate: false,
  skip_health_monitor: false,
  placement_seed: 0,
} as const;

export const runtimeDeployPreviewSchema = Schema.Struct({
  storage: Schema.optional(Schema.mutable(Schema.Array(Schema.Json))),
  project_name: Schema.String.check(Schema.isNonEmpty()),
  prune_refusal: Schema.optionalKey(Schema.NullOr(Schema.Literals([
    "incomplete_snapshot", "selected_services", "filtered_profiles", "guessed_project_name",
  ]))),
  operations: Schema.mutable(Schema.Array(Schema.Json)),
  warnings: Schema.mutable(Schema.Array(Schema.Json)),
  would_remove: Schema.mutable(Schema.Array(Schema.Json)),
  volumes_to_create: Schema.optional(
    Schema.mutable(Schema.Array(Schema.Json)),
  ),
  preserved_volumes: Schema.mutable(Schema.Array(Schema.Json)),
});

export const runtimeDeployOutcomeSchema = Schema.Union([
  Schema.Struct({ type: Schema.Literal("success") }),
  Schema.Struct({ type: Schema.Literal("failed") }),
]);

export type SdkDeployPreview = EnvironmentDeploymentPreview;

function mutableJsonArray(values: readonly Schema.Json[]): JsonValue[] | null {
  const projected = values.map(projectJsonValue);
  if (projected.some((value) => value === undefined)) {
    return null;
  }
  // SAFETY: the guard above proves every projected item is a JsonValue.
  return projected as JsonValue[];
}

export function projectRuntimeDeployPreview(
  decoded: typeof runtimeDeployPreviewSchema.Type,
): SdkDeployPreview | null {
  const storage = decoded.storage === undefined
    ? undefined
    : mutableJsonArray(decoded.storage);
  const operations = mutableJsonArray(decoded.operations);
  const warnings = mutableJsonArray(decoded.warnings);
  const wouldRemove = mutableJsonArray(decoded.would_remove);
  const volumesToCreate =
    decoded.volumes_to_create === undefined
      ? undefined
      : mutableJsonArray(decoded.volumes_to_create);
  const preservedVolumes = mutableJsonArray(decoded.preserved_volumes);
  if (
    storage === null ||
    operations === null ||
    warnings === null ||
    wouldRemove === null ||
    volumesToCreate === null ||
    preservedVolumes === null
  ) {
    return null;
  }
  const preview: SdkDeployPreview = {
    project_name: decoded.project_name,
    operations,
    warnings,
    would_remove: wouldRemove,
    preserved_volumes: preservedVolumes,
  };
  if (storage !== undefined) {
    preview.storage = storage;
  }
  if (decoded.prune_refusal !== undefined) {
    preview.prune_refusal = decoded.prune_refusal;
  }
  if (volumesToCreate !== undefined) {
    preview.volumes_to_create = volumesToCreate;
  }
  return preview;
}

function asProjectName(value: string): ProjectName {
  return value;
}

function shellCommand(command: string): [string, string, string] {
  return ["/bin/sh", "-c", command];
}

function requestedVolume(volumeResourceId: string) {
  const name = getVolumePhysicalName(volumeResourceId);
  return {
    reference: name,
    source: {
      kind: "ordinary" as const,
      name,
      driver: { name: "local", options: {} },
      labels: {},
    },
  };
}

function compileImageSpec(
  snapshot: EnvironmentDeploySnapshot,
  activeVolumeIds: ReadonlySet<string>,
): RequestedServiceSpec {
  const volumes = snapshot.config.mounts.flatMap((mount) =>
    activeVolumeIds.has(mount.volumeResourceId)
      ? [requestedVolume(mount.volumeResourceId)]
      : [],
  );
  const mounts = snapshot.config.mounts.flatMap((mount) =>
    activeVolumeIds.has(mount.volumeResourceId)
      ? [
          {
            volume: getVolumePhysicalName(mount.volumeResourceId),
            target: mount.mountPath,
            read_only: false,
            no_copy: false,
            subpath: null,
          },
        ]
      : [],
  );
  if (snapshot.config.source.type !== "image") {
    throw new UnsupportedDeploymentSourceError(
      `Service ${snapshot.serviceSlug} is not a pullable image.`,
    );
  }
  const spec: RequestedServiceSpec = {
    name: snapshot.config.privateDns,
    mode: {
      mode: "replicated",
      replicas: snapshot.replicas ?? snapshot.config.replicas ?? 1,
    },
    placement: { machines: [] },
    configs: [],
    pre_deploy: null,
    ingress_proxy_fragment: null,
    update: { order: null, monitor_millis: null },
    container: {
      config_mounts: [],
      entrypoint: [],
      labels: {},
      hostname: null,
      extra_hosts: [],
      cap_add: [],
      cap_drop: [],
      healthcheck: null,
      init: null,
      user: null,
      working_directory: null,
      tty: false,
      open_stdin: false,
      privileged: false,
      pid_mode: null,
      log_driver: null,
      resources: {
        cpu_nanos: null, memory_bytes: null, memory_reservation_bytes: null,
        shared_memory_bytes: null, devices: [], device_reservations: [], ulimits: {},
      },
      stop_timeout_secs: null,
      sysctls: {},
      restart: { name: "unless-stopped" },
      image: snapshot.config.source.image,
      environment: snapshot.resolvedEnv ?? {},
      pull_policy: "missing",
      command:
        snapshot.config.startCommand === null
          ? []
          : shellCommand(snapshot.config.startCommand),
    },
    volumes,
    mounts,
    ports: [],
  };
  if (snapshot.config.preDeployCommand !== null) {
    spec.pre_deploy = {
      environment: {}, privileged: null, timeout_millis: null, user: null,
      command: shellCommand(snapshot.config.preDeployCommand),
    };
  }
  return spec;
}

export function compileSdkDeployIntent(input: {
  projectName: string;
  snapshots: readonly EnvironmentDeploySnapshot[];
  volumes?: readonly EnvironmentDeployVolume[];
}): DeployIntent {
  const volumeIds = new Set(
    (input.volumes ?? []).map((volume) => volume.volumeResourceId),
  );
  const target: RequestedServiceSpec[] = [];
  const selected: Array<{ name: RequestedServiceSpec["name"] }> = [];
  for (const snapshot of input.snapshots) {
    switch (snapshot.config.source.type) {
      case "empty":
        break;
      case "git":
        throw new UnsupportedDeploymentSourceError(
          `Git service ${snapshot.serviceSlug} is missing a pullable image.`,
        );
      case "image": {
        const spec = compileImageSpec(snapshot, volumeIds);
        target.push(spec);
        selected.push({ name: spec.name });
        break;
      }
      default: {
        const exhaustive: never = snapshot.config.source;
        return exhaustive;
      }
    }
  }
  return {
    project_name: asProjectName(input.projectName),
    target,
    options: {
      ...DEFAULT_PLAN_OPTIONS,
      selected,
    },
  };
}

export function parseSdkDeployPreview<T>(value: T): SdkDeployPreview {
  const preview = projectRuntimeDeployPreview(
    Schema.decodeUnknownSync(runtimeDeployPreviewSchema)(
      value,
      strictParseOptions,
    ),
  );
  if (preview === null) {
    throw new Error("SDK deploy preview contains non-JSON data.");
  }
  return preview;
}

export function requireConfirmableSdkDeployPreview(input: {
  status: string;
  preview: unknown;
}) {
  if (input.status !== "planning") {
    return Effect.fail(
      new Conflict({
        message: "The deployment is not waiting for confirm.",
      }),
    );
  }
  return Schema.decodeUnknownEffect(runtimeDeployPreviewSchema)(
    input.preview,
    strictParseOptions,
  ).pipe(
    Effect.mapError(
      () =>
        new Conflict({
          message: "The deployment has no confirmable preview. Preview again.",
        }),
    ),
    Effect.flatMap((decoded) => {
      const preview = projectRuntimeDeployPreview(decoded);
      return preview === null
        ? Effect.fail(
            new Conflict({
              message: "The deployment has no confirmable preview. Preview again.",
            }),
          )
        : Effect.succeed(preview);
    }),
  );
}
