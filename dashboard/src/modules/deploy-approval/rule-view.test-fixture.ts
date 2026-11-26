import type { RuleView } from "@ployz/sdk";
import { versionSteps } from "@ployz/sdk/authority";
import { asTestDouble } from "#/lib/test-double";
import type { DeployTap } from "./deploy-tap.server";

export const CLUSTER = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";
const SEALED = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";

const spec = (name: string) => ({
  name,
  mode: { mode: "replicated", replicas: 1 },
  container: {
    image: `ployz-build/${name}`,
    pull_policy: "never",
    environment: { SECRET: { sealed: SEALED, scope: "shared", key: "SECRET" } },
    sealed_identity: { organization: "org", environment: "11111111-1111-1111-1111-111111111111", service: "22222222-2222-2222-2222-222222222222" },
  },
});

/** web with a Volume, which runs only on the Servers a rule names. */
export const placed = () => ({
  ...spec("web"),
  volumes: [{ reference: "data", source: { kind: "provisioned", name: "web-data", maximum_bytes: 1024 } }],
  mounts: [{ volume: "data", target: "/data" }],
});

/** The Store's staged rule view of a Save of web, which builds from main, at review `v1`. */
export const view = (rule: RuleView["rule"] = null, web: ReturnType<typeof spec> = spec("web")): RuleView =>
  // SAFETY: the fixture is the JSON the Store answers; ployz-core parses what it reads of it.
  JSON.parse(JSON.stringify({
    environment: { id: "env", project: "shop", name: "production", revision: 1 },
    namespace: "app",
    rule,
    sources: [{
      service: "web", repository: "acme/web", repository_id: 11, branch: "main", root_dir: ".",
      build: { buildMethod: "dockerfile", dockerfilePath: "Dockerfile" },
    }],
    target: [web],
    changed: [],
    version: "v1",
    protected: false,
  })) as RuleView;

/** The rule `web` gets when a Save's tap signs `rule` for `view(null, web)`, kept as the Store keeps it. */
export const signedRule = (web: ReturnType<typeof spec> = spec("web")) => {
  const { sources, target } = view(null, web);
  const draft = versionSteps({
    cluster: CLUSTER, namespace: "app", services: [], operation: "4".repeat(32), issued_at: 1_000, expires_at: 601_000,
    seed: Array.from({ length: 16 }, () => 3), against: { trusted: { devices: [], passkeys: [], grants: [] }, containers: [] },
    source: { rule: null, sources, target, machines: ["1".repeat(32)], owner_id: 7 },
  });
  if (draft?.source === undefined) throw new Error("a first Save signs a rule");
  return { body: draft.source.body, signatures: [] };
};

export const kept = () => signedRule();

/** A gate every enforcing Server answered, trusting no passkey of the person's in particular. */
export const ready = asTestDouble<Extract<DeployTap, { kind: "ready" }>>()({
  kind: "ready", cluster: CLUSTER, passkeys: [], pins: {}, members: ["1".repeat(32)],
  against: { trusted: { devices: [], passkeys: [], grants: [] }, containers: [] },
});
