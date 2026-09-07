import { describe, expect, it } from "vitest";
import { Effect } from "effect";
import {
  decodeFrozenDeployInput as decodeFrozenDeployInputEffect,
  encodeFrozenDeployInput as encodeFrozenDeployInputEffect,
  redactFrozenDeployManifest,
  type FrozenDeployInput,
} from "#/modules/deployments/frozen-input.server";
import { makeSecretEncryption } from "#/utils/encrypted-secret.server";

const encryption = makeSecretEncryption("test-encryption-secret");

const TestResult = {
  ok: <A>(value: A) => ({ status: "ok" as const, value }),
  err: <E>(error: E) => ({ status: "error" as const, error }),
  isOk: <A, E>(
    result: { status: "ok"; value: A } | { status: "error"; error: E },
  ) => result.status === "ok",
  isError: <A, E>(
    result: { status: "ok"; value: A } | { status: "error"; error: E },
  ) => result.status === "error",
};

function encryptedInput<T>(payload: T) {
  return encryption.encrypt(JSON.stringify(payload));
}

function syncResult<A, E>(program: Effect.Effect<A, E>) {
  return Effect.runSync(
    Effect.match(program, {
      onFailure: (error) => TestResult.err(error),
      onSuccess: (value) => TestResult.ok(value),
    }),
  );
}

function decodeFrozenDeployInput(
  value: Parameters<typeof decodeFrozenDeployInputEffect>[1],
) {
  return syncResult(decodeFrozenDeployInputEffect(encryption, value));
}

function encodeFrozenDeployInput(input: FrozenDeployInput) {
  return syncResult(encodeFrozenDeployInputEffect(encryption, input));
}

describe("frozen deploy input codec", () => {
  it("restores a deeply validated v1 artifact and keeps secrets encrypted", () => {
    const encrypted = encryptedInput({
      version: 1,
      target: {
        namespace_id: "production",
        services: [
          {
            service_id: "api",
            image: "ghcr.io/acme/api:sha",
            mode: { kind: "replicated", replicas: 1 },
            runtime: {
              command: null,
              entrypoint: null,
              environment: { API_TOKEN: "secret-value" },
              stop_grace_period: 10,
            },
          },
        ],
      },
      registryCredentials: {
        api: { kind: "identity_token", token: "registry-secret" },
      },
      volumeCount: 0,
    });

    const decoded = decodeFrozenDeployInput(encrypted);
    expect(TestResult.isOk(decoded)).toBe(true);
    if (TestResult.isError(decoded) || !decoded.value) return;
    expect(decoded.value.request.target.namespace_id).toBe(
      "production",
    );
    expect(decoded.value.request.target.services[0]).toMatchObject({
      service_id: "api",
      image: "ghcr.io/acme/api:sha",
      mode: { kind: "replicated", replicas: 1 },
    });
    expect(
      JSON.stringify(redactFrozenDeployManifest(decoded.value)),
    ).not.toContain("secret-value");
    expect(
      JSON.stringify(redactFrozenDeployManifest(decoded.value)),
    ).not.toContain("registry-secret");

    const encoded = encodeFrozenDeployInput(decoded.value);
    expect(TestResult.isOk(encoded)).toBe(true);
    expect(
      JSON.stringify(TestResult.isOk(encoded) ? encoded.value : null),
    ).not.toContain("secret-value");
  });

  it("fails closed on deep schema drift", () => {
    const encrypted = encryptedInput({
      version: 1,
      target: {
        namespace_id: "production",
        services: [
          {
            service_id: "api",
            image: "ghcr.io/acme/api:sha",
            mode: { kind: "replicated", replicas: 1 },
            runtime: {
              command: null,
              entrypoint: null,
              environment: {},
              stop_grace_period: 10,
              routes: "misplaced",
            },
          },
        ],
      },
      registryCredentials: {},
      volumeCount: 0,
    });

    expect(TestResult.isError(decodeFrozenDeployInput(encrypted))).toBe(true);
  });

  it("restores the alpha.66 pushed-image source layout", () => {
    const encrypted = encryptedInput({
      version: 1,
      target: {
        namespace_id: "production",
        services: [
          {
            service_id: "api",
            image: "sha256:index",
            image_source: {
              source: "pushed_to_seed",
              index_digest: "sha256:index",
              platforms: [
                [
                  { os: "linux", architecture: "amd64" },
                  {
                    seed: "seed-1",
                    manifest_digest: "sha256:manifest",
                    image_id: "sha256:image",
                    availability_expires_at: "2000000000",
                  },
                ],
              ],
            },
            mode: { kind: "replicated", replicas: 1 },
            runtime: {
              command: null,
              entrypoint: null,
              environment: {},
              stop_grace_period: 10,
            },
          },
        ],
      },
      registryCredentials: {},
      volumeCount: 0,
    });

    const decoded = decodeFrozenDeployInput(encrypted);
    expect(TestResult.isOk(decoded)).toBe(true);
    if (TestResult.isError(decoded) || !decoded.value) return;
    expect(decoded.value.request.target.services[0]?.image_source).toEqual({
      source: "pushed_to_seed",
      index_digest: "sha256:index",
      platforms: [
        [
          { os: "linux", architecture: "amd64" },
          {
            seed: "seed-1",
            manifest_digest: "sha256:manifest",
            image_id: "sha256:image",
            availability_expires_at: "2000000000",
          },
        ],
      ],
    });
  });

  it("migrates v1 mounted volumes to explicit plain declarations", () => {
    const encrypted = encryptedInput({
      version: 1,
      target: {
        namespace_id: "production",
        services: [
          {
            service_id: "api",
            image: "nginx:stable",
            mode: { kind: "replicated", replicas: 1 },
            runtime: {
              command: null,
              entrypoint: null,
              environment: {},
              stop_grace_period: 10,
              volume_mounts: [{ volume_name: "data", target: "/data" }],
            },
          },
        ],
      },
      registryCredentials: {},
      volumeCount: 1,
    });

    const decoded = decodeFrozenDeployInput(encrypted);
    expect(TestResult.isOk(decoded)).toBe(true);
    if (TestResult.isError(decoded) || !decoded.value) return;
    expect(decoded.value.version).toBe(3);
    expect(decoded.value.request.target.volumes).toEqual({
      data: { kind: "plain" },
    });
    expect(decoded.value.request.phases).toEqual([
      {
        services: [{ service_id: "api", requirement: "required" }],
      },
    ]);

    const encoded = encodeFrozenDeployInput(decoded.value);
    expect(TestResult.isOk(encoded)).toBe(true);
    if (TestResult.isError(encoded)) return;
    const roundTripped = decodeFrozenDeployInput(encoded.value);
    expect(TestResult.isOk(roundTripped)).toBe(true);
    if (TestResult.isError(roundTripped) || !roundTripped.value) return;
    expect(roundTripped.value.request.target.volumes).toEqual({
      data: { kind: "plain" },
    });
  });

  it("reads historical provisioned volume declarations as named Docker volumes", () => {
    const encrypted = encryptedInput({
      version: 2,
      target: {
        namespace_id: "production",
        volumes: {
          data: { kind: "provisioned", max_size_bytes: 10_737_418_240 },
        },
        services: [],
      },
      registryCredentials: {},
      volumeCount: 1,
    });

    const decoded = decodeFrozenDeployInput(encrypted);
    expect(TestResult.isOk(decoded)).toBe(true);
    if (TestResult.isError(decoded) || !decoded.value) return;
    expect(decoded.value.request.target.volumes).toEqual({
      data: { kind: "plain" },
    });
  });

  it("round-trips a v3 artifact with the complete phase-aware request", () => {
    const encrypted = encryptedInput({
      version: 3,
      request: {
        version: 1,
        target: {
          namespace_id: "production",
          volumes: {},
          services: [],
        },
        phases: [
          {
            services: [
              {
                service_id: "retired-worker",
                requirement: "opportunistic",
              },
            ],
          },
        ],
      },
      registryCredentials: {},
      volumeCount: 0,
    });

    const decoded = decodeFrozenDeployInput(encrypted);
    expect(TestResult.isOk(decoded)).toBe(true);
    if (TestResult.isError(decoded) || !decoded.value) return;
    expect(decoded.value.request).toMatchObject({
      version: 1,
      target: { namespace_id: "production", services: [] },
      phases: [
        {
          services: [
            {
              service_id: "retired-worker",
              requirement: "opportunistic",
            },
          ],
        },
      ],
    });
    expect(TestResult.isOk(encodeFrozenDeployInput(decoded.value))).toBe(true);
  });

  it("rejects duplicate Service actions in a v3 artifact", () => {
    const encrypted = encryptedInput({
      version: 3,
      request: {
        version: 1,
        target: {
          namespace_id: "production",
          volumes: {},
          services: [],
        },
        phases: [
          {
            services: [
              { service_id: "retired", requirement: "opportunistic" },
            ],
          },
          {
            services: [{ service_id: "retired", requirement: "required" }],
          },
        ],
      },
      registryCredentials: {},
      volumeCount: 0,
    });

    expect(TestResult.isError(decodeFrozenDeployInput(encrypted))).toBe(true);
  });

  it("rejects the never-produced legacy pushed-image source layout", () => {
    const encrypted = encryptedInput({
      version: 1,
      target: {
        namespace_id: "production",
        services: [
          {
            service_id: "api",
            image: "sha256:image",
            image_source: {
              source: "pushed_to_seed",
              seed: "seed-1",
              manifest_digest: "sha256:manifest",
              image_id: "sha256:image",
            },
            mode: { kind: "replicated", replicas: 1 },
            runtime: {
              command: null,
              entrypoint: null,
              environment: {},
              stop_grace_period: 10,
            },
          },
        ],
      },
      registryCredentials: {},
      volumeCount: 0,
    });

    expect(TestResult.isError(decodeFrozenDeployInput(encrypted))).toBe(true);
  });

  it("returns a typed error when a volume mount target is not absolute", () => {
    const encrypted = encryptedInput({
      version: 1,
      target: {
        namespace_id: "production",
        services: [
          {
            service_id: "api",
            image: "ghcr.io/acme/api:sha",
            mode: { kind: "replicated", replicas: 1 },
            runtime: {
              command: null,
              entrypoint: null,
              environment: {},
              stop_grace_period: 10,
              volume_mounts: [{ volume_name: "data", target: "relative" }],
            },
          },
        ],
      },
      registryCredentials: {},
      volumeCount: 1,
    });

    const decoded = decodeFrozenDeployInput(encrypted);
    expect(TestResult.isError(decoded)).toBe(true);
    if (TestResult.isError(decoded)) {
      expect(decoded.error).toMatchObject({
        _tag: "FrozenDeployInputError",
        failureCode: "frozen_input_invalid",
      });
    }
  });

  it("requires an explicit migration branch for future versions", () => {
    const encrypted = encryptedInput({ version: 4, request: {} });

    const decoded = decodeFrozenDeployInput(encrypted);
    expect(TestResult.isError(decoded)).toBe(true);
    if (TestResult.isError(decoded)) {
      expect(decoded.error.message).toContain("version is unsupported");
    }
  });
});
