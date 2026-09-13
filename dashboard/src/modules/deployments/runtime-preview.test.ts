import { resolvedServiceSpecFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { describe, expect, it } from "vitest";
import type {
  DeployPreview,
  MachineId,
  OperationRow,
} from "@ployz/sdk";
import {
  compileSdkDeployIntent,
  parseSdkDeployPreview,
} from "#/modules/deployments/runtime-preview";

import {
  createDefaultServiceHealthcheck,
  createDefaultServiceRestartPolicy,
  createEmptyServiceSource,
  createGitServiceSource,
  createImageServiceSource,
  projectServiceDeploymentConfig,
} from "#/modules/environment-design/services";

const volumeResourceId = "00000000-0000-4000-8000-000000000201";
const tombstonedVolumeResourceId = "00000000-0000-4000-8000-000000000202";
const machineId = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId;

const rustOperation: OperationRow["operation"] = {
  type: "run_container",
  machine_id: machineId,
  spec: resolvedServiceSpecFixture(),
  skip_health_monitor: false,
};

const rustPreview: DeployPreview = {
  storage: [],
  prune_refusal: null,
  project_name: "production",
  operations: [
    {
      machine_name: null,
      display_name: null,
      service_name: "api",
      index: 0,
      machine_id: machineId,
      operation: rustOperation,
      status: { type: "pending" },
    },
  ],
  warnings: [],
  would_remove: [],
  volumes_to_create: [{ machine_id: machineId, machine_name: null, name: "data", maximum_bytes: null }],
  preserved_volumes: [],
};

function imageSnapshot(input?: {
  privateDns?: string;
  image?: string;
  env?: Record<string, string>;
  mounts?: Array<{ volumeResourceId: string; volumeName: string; mountPath: string }>;
}) {
  const privateDns = input?.privateDns ?? "api";
  return {
    serviceId: "service-api",
    serviceSlug: privateDns,
    config: projectServiceDeploymentConfig({
      name: privateDns,
      privateDns,
      source: createImageServiceSource({
        image: input?.image ?? "nginx:1.27",
      }),
      preDeployCommand: null,
      startCommand: null,
      healthcheck: createDefaultServiceHealthcheck(),
      restartPolicy: createDefaultServiceRestartPolicy(),
      env: {},
      mounts: input?.mounts ?? [],
    }),
    resolvedEnv: input?.env,
  };
}

describe("compileSdkDeployIntent", () => {
  it("compiles an image service with decrypted env into rust DeployIntent wire JSON", () => {
    const intent = compileSdkDeployIntent({
      projectName: "production",
      snapshots: [
        imageSnapshot({
          env: { API_KEY: "decrypted-secret", PORT: "8080" },
          mounts: [
            {
              volumeResourceId,
              volumeName: "data",
              mountPath: "/data",
            },
          ],
        }),
      ],
      volumes: [
        { volumeResourceId },
        { volumeResourceId: tombstonedVolumeResourceId },
      ],
    });

    expect(intent).toEqual({
      project_name: "production",
      dependencies: {},
      service_profiles: {},
      requested_profiles: [],
      compose_refusal: null,
      target: [
        {
          name: "api",
          mode: { mode: "replicated", replicas: 1 },
          placement: { constraints: [] },
          configs: [], pre_deploy: null, ingress_proxy_fragment: null,
          update: { order: null, monitor_millis: null },
          container: {
            ...resolvedServiceSpecFixture().container,
            image: "nginx:1.27",
            environment: { API_KEY: "decrypted-secret", PORT: "8080" },
            pull_policy: "missing",
            command: [],
          },
          volumes: [
            {
              reference: "vol-00000000-0000-4000-8000-000000000201",
              source: {
                kind: "ordinary",
                name: "vol-00000000-0000-4000-8000-000000000201",
                driver: { name: "local", options: {} },
                labels: {},
              },
            },
          ],
          mounts: [
            {
              volume: "vol-00000000-0000-4000-8000-000000000201",
              target: "/data",
              read_only: false, no_copy: false, subpath: null,
            },
          ],
          ports: [],
        },
      ],
      options: {
        force_recreate: false,
        skip_health_monitor: false,
        placement_seed: 0,
        selected: [{ name: "api" }],
      },
    });
  });

  it("omits empty services and unmounted volumes from the intent", () => {
    const intent = compileSdkDeployIntent({
      projectName: "production",
      snapshots: [
        imageSnapshot(),
        {
          serviceId: "service-empty",
          serviceSlug: "placeholder",
          config: projectServiceDeploymentConfig({
            name: "placeholder",
            privateDns: "placeholder",
            source: createEmptyServiceSource(),
            preDeployCommand: null,
            startCommand: null,
            healthcheck: createDefaultServiceHealthcheck(),
            restartPolicy: createDefaultServiceRestartPolicy(),
          }),
        },
      ],
      volumes: [{ volumeResourceId: tombstonedVolumeResourceId }],
    });

    expect(intent.options.selected).toEqual([{ name: "api" }]);
    expect(intent.target).toHaveLength(1);
    const spec = intent.target[0];
    expect(spec).toMatchObject({ name: "api" });
    if (spec === undefined) {
      throw new Error("expected a requested service spec object");
    }
    expect(spec.volumes).toEqual([]);
    expect(spec.mounts).toEqual([]);
  });

  it("refuses to compile git-as-source instead of inventing an image", () => {
    expect(() =>
      compileSdkDeployIntent({
        projectName: "production",
        snapshots: [
          {
            serviceId: "service-git",
            serviceSlug: "api",
            config: projectServiceDeploymentConfig({
              name: "api",
              privateDns: "api",
              source: createGitServiceSource({
                repository: "acme/api",
                repositoryId: 42,
                installationId: 7,
              }),
              preDeployCommand: null,
              startCommand: null,
              healthcheck: createDefaultServiceHealthcheck(),
              restartPolicy: createDefaultServiceRestartPolicy(),
            }),
          },
        ],
        volumes: [],
      }),
    ).toThrow("missing a pullable image");
  });
});

describe("parseSdkDeployPreview", () => {
  it("accepts current SDK previews and rejects malformed operation or volume details", () => {
    expect(parseSdkDeployPreview(rustPreview)).toEqual(rustPreview);
    expect(() => parseSdkDeployPreview({ ...rustPreview, storage: {} })).toThrow();
    expect(() => parseSdkDeployPreview({ ...rustPreview, volumes_to_create: {} })).toThrow();
    expect(() => parseSdkDeployPreview({ ...rustPreview, operations: [{ type: "unknown" }] })).toThrow();
  });

});

it.each(["incomplete_snapshot", "selected_services", "filtered_profiles", "guessed_project_name"] as const)(
  "preserves SDK prune refusal %s", (prune_refusal) => {
    expect(parseSdkDeployPreview({ ...rustPreview, prune_refusal }).prune_refusal).toBe(prune_refusal);
  },
);
