import { describe, expect, it } from "vitest";
import { Effect } from "effect";
import type { JsonObject } from "#/db/schema";
import {
  compileSavedEnvironmentIntent,
  savedEnvironmentIntentSchema,
  type SavedEnvironmentIntent,
} from "#/modules/environment-design/saved-intent";
import {
  discardSavedServiceIntentSetting,
  replaceSavedEnvironmentIntentNode,
} from "#/modules/environment-design/saved-intent-mutations";
import { projectJsonObject } from "#/lib/json";
import {
  decodeStrict,
  isValid,
  type DeepMutable,
} from "#/modules/environment-design/schema";

const environmentId = "00000000-0000-4000-8000-000000000001";
const serviceId = "00000000-0000-4000-8000-000000000002";
const serviceLineageId = "00000000-0000-4000-8000-000000000003";
const resourceId = "00000000-0000-4000-8000-000000000004";
const resourceLineageId = "00000000-0000-4000-8000-000000000005";
const groupId = "00000000-0000-4000-8000-000000000006";
const groupLineageId = "00000000-0000-4000-8000-000000000007";
const variableId = "00000000-0000-4000-8000-000000000008";
const volumeId = "00000000-0000-4000-8000-000000000009";
const volumeLineageId = "00000000-0000-4000-8000-000000000010";

function intent(value: string): DeepMutable<SavedEnvironmentIntent> {
  return decodeStrict(savedEnvironmentIntentSchema, {
    version: 1,
    environmentSlug: "production",
    services: [
      {
        id: serviceId,
        lineageId: serviceLineageId,
        slug: "web",
        config: {
          version: 2,
          name: "Web",
          privateDns: "web",
          source: { version: 1, type: "empty", rootDir: "/" },
          preDeployCommand: null,
          startCommand: null,
          healthcheck: { type: "none" },
          restartPolicy: "unless-stopped",
        },
        variables: [],
        variableGroupAttachments: [{ variableGroupId: groupId, sortOrder: 0 }],
        volumeAttachments: [],
        encryptedRegistryUsername: null,
        encryptedRegistrySecret: null,
      },
    ],
    variableGroups: [
      {
        resourceId,
        resourceLineageId,
        variableGroupId: groupId,
        variableGroupLineageId: groupLineageId,
        slug: "shared",
        name: "Shared",
        variables: [
          {
            id: variableId,
            key: "SHARED_VALUE",
            description: null,
            exported: true,
            valueFingerprint: `fingerprint-${value}`,
            value: { kind: "literal", value },
          },
        ],
      },
    ],
    volumes: [],
  });
}

describe("Saved Environment intent boundary", () => {
  it("rejects deployment artifacts from canonical Saved State", () => {
    const withServiceArtifact = (artifact: JsonObject) => {
      const candidate = projectJsonObject(structuredClone(intent("new")));
      if (candidate === null) throw new Error("expected json object");
      const services = candidate["services"] as Array<JsonObject>;
      services[0] = {
        ...services[0],
        config: {
          ...(services[0]?.["config"] as JsonObject),
          ...artifact,
        },
      };
      return candidate;
    };
    const withProducers = projectJsonObject(structuredClone(intent("new")));
    if (withProducers === null) throw new Error("expected json object");
    withProducers["variableProducers"] = [];

    expect(
      isValid(savedEnvironmentIntentSchema, withServiceArtifact({ env: {} })),
    ).toBe(false);
    expect(
      isValid(
        savedEnvironmentIntentSchema,
        withServiceArtifact({ mounts: [] }),
      ),
    ).toBe(false);
    expect(isValid(savedEnvironmentIntentSchema, withProducers)).toBe(false);
  });

  it("rejects ambiguous graph identities before compilation", () => {
    const candidate = intent("new");
    const service = candidate.services[0];
    if (!service) throw new Error("missing service");
    candidate.services.push(structuredClone(service));

    expect(isValid(savedEnvironmentIntentSchema, candidate)).toBe(false);
  });

  it("rejects setting paths outside the generated discardable diff", () => {
    const current = intent("new");
    const baseline = intent("applied");
    const service = current.services[0];
    const baselineService = baseline.services[0];
    if (!service || !baselineService) throw new Error("missing service");
    service.config.name = "New name";
    baselineService.config.name = "Applied name";

    expect(() =>
      Effect.runSync(
        discardSavedServiceIntentSetting({
          current,
          baseline,
          serviceId,
          path: "env.NOT_AN_AUTHORED_SETTING",
        }),
      ),
    ).toThrow();
  });

  it("recompiles every consumer from the replaced owning node", () => {
    const discarded = Effect.runSync(
      replaceSavedEnvironmentIntentNode({
        current: intent("new"),
        baseline: intent("applied"),
        node: { nodeType: "variable_group", nodeId: resourceId },
      }),
    );
    const compiled = compileSavedEnvironmentIntent({
      environmentId,
      intent: discarded,
    });
    const service = compiled.nodeSnapshots.find(
      (node) => node.nodeType === "service" && node.nodeId === serviceId,
    );
    const producer = compiled.variableProducers.find(
      (candidate) =>
        candidate.ownerScope === "variable_group" &&
        candidate.ownerId === groupId &&
        candidate.key === "SHARED_VALUE",
    );

    expect(service?.config).toMatchObject({
      env: {
        SHARED_VALUE: {
          kind: "literal",
          value: "applied",
          source: { kind: "variable_group", variableGroupId: groupId },
        },
      },
    });
    expect(producer?.value).toEqual({ kind: "literal", value: "applied" });
  });

  it("restores owner edges before recompiling a discarded deletion", () => {
    const baseline = intent("applied");
    baseline.volumes = [
      {
        resourceId: volumeId,
        resourceLineageId: volumeLineageId,
        name: "Data",
      },
    ];
    baseline.services[0]?.volumeAttachments.push({
      volumeResourceId: volumeId,
      mountPath: "/data",
    });
    const afterDeletion = structuredClone(baseline);
    afterDeletion.variableGroups = [];
    afterDeletion.volumes = [];
    if (afterDeletion.services[0]) {
      afterDeletion.services[0].variableGroupAttachments = [];
      afterDeletion.services[0].volumeAttachments = [];
    }

    const restoredGroup = Effect.runSync(
      replaceSavedEnvironmentIntentNode({
        current: afterDeletion,
        baseline,
        node: { nodeType: "variable_group", nodeId: resourceId },
      }),
    );
    const restoredVolume = Effect.runSync(
      replaceSavedEnvironmentIntentNode({
        current: restoredGroup,
        baseline,
        node: { nodeType: "volume", nodeId: volumeId },
      }),
    );

    const compiled = compileSavedEnvironmentIntent({
      environmentId,
      intent: restoredVolume,
    });
    const service = compiled.nodeSnapshots.find(
      (node) => node.nodeType === "service" && node.nodeId === serviceId,
    );
    expect(service?.config).toMatchObject({
      env: { SHARED_VALUE: { value: "applied" } },
      mounts: [
        { volumeResourceId: volumeId, volumeName: "Data", mountPath: "/data" },
      ],
    });
  });

  it("retains references to deleted owners for warning-aware resolution", () => {
    const current = intent("applied");
    const group = current.variableGroups[0];
    if (!group) throw new Error("missing group");
    const variable = group.variables[0];
    if (!variable) throw new Error("missing variable");
    group.variables[0] = {
      ...variable,
      value: {
        kind: "template",
        parts: [
          {
            kind: "ref",
            owner: { scope: "service", lineageId: serviceLineageId },
            key: "PLOYZ_SERVICE_NAME",
          },
        ],
      },
    };

    const discarded = Effect.runSync(
      replaceSavedEnvironmentIntentNode({
        current,
        baseline: null,
        node: { nodeType: "service", nodeId: serviceId },
      }),
    );
    expect(discarded.variableGroups[0]?.variables[0]?.value).toEqual(
      group.variables[0]?.value,
    );
  });

  it("canonicalizes compiled mounts by mount path", () => {
    const current = intent("value");
    const earlierPathVolumeId = "00000000-0000-4000-8000-000000000099";
    current.volumes = [
      {
        resourceId: volumeId,
        resourceLineageId: volumeLineageId,
        name: "Later",
      },
      {
        resourceId: earlierPathVolumeId,
        resourceLineageId: "00000000-0000-4000-8000-000000000098",
        name: "Earlier",
      },
    ];
    current.services[0]?.volumeAttachments.push(
      { volumeResourceId: volumeId, mountPath: "/z" },
      { volumeResourceId: earlierPathVolumeId, mountPath: "/a" },
    );

    const compiled = compileSavedEnvironmentIntent({
      environmentId,
      intent: current,
    });
    const service = compiled.nodeSnapshots.find(
      (node) => node.nodeType === "service",
    );
    expect(service?.config).toMatchObject({
      mounts: [{ mountPath: "/a" }, { mountPath: "/z" }],
    });
  });
});
