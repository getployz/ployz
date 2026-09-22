import { Effect } from "effect";
import { expect, it } from "vitest";
import { getResolvedDeployEnvBySnapshotConfig } from "./deploy-environment.server";
import { makeSecretEncryption } from "#/utils/encrypted-secret.server";

it("resolves authored values and leaves service defaults to Core", async () => {
  const encryption = makeSecretEncryption("test-encryption-secret");
  const env = await Effect.runPromise(getResolvedDeployEnvBySnapshotConfig(
    encryption,
    [
      { serviceId: "default", config: { routes: [], managedHostnames: [], env: {} } },
      { serviceId: "explicit", config: { routes: [], managedHostnames: [], env: { PORT: { kind: "literal", value: "80" } } } },
      { serviceId: "empty", config: { routes: [], managedHostnames: [], env: { PORT: { kind: "literal", value: "" } } } },
      { serviceId: "secret", config: { routes: [], managedHostnames: [], env: { PORT: { kind: "secret", fingerprint: encryption.sealedFingerprint("3000"), encryptedValue: encryption.encrypt("3000") } } } },
    ],
    null,
  ));
  expect(Object.fromEntries(env)).toEqual({
    default: {},
    explicit: { PORT: "80" },
    empty: { PORT: "" },
    secret: { PORT: "3000" },
  });
});

it("resolves self and cross-service public domain references from captured routes", async () => {
  const template = (owner: { scope: "self" } | { scope: "service"; lineageId: string }) => ({
    kind: "literal" as const, value: "", parts: [{ kind: "text" as const, value: "https://" }, { kind: "ref" as const, owner, key: "PLOYZ_PUBLIC_DOMAIN" }],
  });
  const snapshots: Parameters<typeof getResolvedDeployEnvBySnapshotConfig>[1] = [
    { serviceId: "api", config: { routes: [{ id: "old", hostname: "old.example.test", targetPort: null }, { id: "new", hostname: "not-resolving.example.test", targetPort: null }], managedHostnames: [{ prefix: "api", targetPort: null }], env: { APP_URL: template({ scope: "self" }) } } },
    { serviceId: "web", config: { routes: [], managedHostnames: [{ prefix: "web", targetPort: null }], env: { API_URL: template({ scope: "service", lineageId: "api-lineage" }), APP_URL: template({ scope: "self" }) } } },
  ];
  const producers = ["api", "web"].map(id => ({ ownerScope: "service" as const, ownerId: id, ownerLineageId: `${id}-lineage`, key: "PLOYZ_SERVICE_ID", value: { kind: "literal" as const, value: id } }));
  const env = await Effect.runPromise(getResolvedDeployEnvBySnapshotConfig(makeSecretEncryption("test-encryption-secret"), snapshots, producers, "cluster.example.test"));
  expect(Object.fromEntries(env)).toEqual({
    api: { PLOYZ_PUBLIC_DOMAIN: "not-resolving.example.test", APP_URL: "https://not-resolving.example.test" },
    web: { PLOYZ_PUBLIC_DOMAIN: "web.cluster.example.test", APP_URL: "https://web.cluster.example.test", API_URL: "https://not-resolving.example.test" },
  });

});

it("preserves authored overrides of the public-domain default", async () => {
  const env = await Effect.runPromise(getResolvedDeployEnvBySnapshotConfig(makeSecretEncryption("test-encryption-secret"), [{ serviceId: "api", config: {
    routes: [{ id: "route", hostname: "managed.example.test", targetPort: null }], managedHostnames: [],
    env: { PLOYZ_PUBLIC_DOMAIN: { kind: "literal", value: "override.example.test" }, APP_URL: { kind: "literal", value: "", parts: [{ kind: "ref", owner: { scope: "self" }, key: "PLOYZ_PUBLIC_DOMAIN" }] } },
  } }], [{ ownerScope: "service", ownerId: "api", ownerLineageId: "api-lineage", key: "PLOYZ_PUBLIC_DOMAIN", value: { kind: "literal", value: "override.example.test" } }]));
  expect(env.get("api")).toEqual({ PLOYZ_PUBLIC_DOMAIN: "override.example.test", APP_URL: "override.example.test" });
});
