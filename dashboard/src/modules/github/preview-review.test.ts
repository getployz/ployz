import type { PreviewReviewView } from "@ployz/sdk";
import { afterEach, describe, expect, it, vi } from "vitest";
import { canonicalJson, hex } from "#/modules/approval-session/approval-session";
import { answerReview, draftReview, ReviewRefused } from "./preview-review";

const CLUSTER = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";
const COMMIT = "fedcba9876543210fedcba9876543210fedcba98";
const NAMESPACE = "pv-1111111111111111-079f47b8f212";
/** `preview_tests::rule()`'s template of web in NAMESPACE, which every Server takes again at creation. */
const TEMPLATE = "61b6d99f3c9ad6040c96c17302a55063829b06e82532faa4d8a5ee540f8e25a5";

/** `preview_tests::rule()`, as the Store keeps it. */
const RULE = {
  cluster: CLUSTER, rule: "1".repeat(32), generation: 2, machines: ["1".repeat(32)], parent: "shop-production",
  parent_id: "11111111-1111-1111-1111-111111111111", domain: "preview.example.com",
  source: {
    repository_id: 1_450_985_696, owner_id: 278_111_222, repository: "Ployz-test/lab-service", branch: "main",
    workflow: ".github/workflows/ployz-build.yml", events: ["push", "workflow_dispatch"],
    builder: { ref: `Ployz-test/build/.github/workflows/build.yml@${"0123456789abcdef".repeat(2)}01234567`, sha: `${"0123456789abcdef".repeat(2)}01234567` },
    runner: "github-hosted",
  },
  trusted: [1234],
  services: [{
    name: "web", template: TEMPLATE, setup: "125ec297dbe0118485d13349cd2af2c2010f7d9ac79b977f17f7b66a77a0310c",
    policy: { method: { dockerfile: { path: "Dockerfile" } }, context: ".", platforms: ["linux/amd64"], cache: "off" },
    inherit: ["DATABASE_URL"], generated: { GEN: { length: 16, charset: "a-z" } },
  }],
  issued_at: 1000, ended: false, signer: "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
};

/** `preview_tests::preview_json(NAMESPACE, "zyxwvutsrqponmlk", false)`: web as the preview's Deploy lowers it. */
const SPEC = {
  name: "web", mode: { mode: "replicated", replicas: 1 },
  container: {
    image: `ployz-build/web@sha256:${"4".repeat(64)}`, command: [], entrypoint: [],
    environment: {
      DATABASE_URL: { sealed: `${"QUFB".repeat(16)}QUFBQQ==`, scope: "shared", key: "DATABASE_URL", environment: "11111111-1111-1111-1111-111111111111" },
      DB: "db.shop-production.internal", GEN: "zyxwvutsrqponmlk",
    },
    sealed_identity: { organization: "org", environment: "33333333-3333-3333-3333-333333333333", service: "22222222-2222-2222-2222-222222222222" },
    labels: {}, hostname: null, extra_hosts: [], cap_add: [], cap_drop: [], healthcheck: null, pull_policy: "never", init: null,
    user: null, working_directory: null, tty: false, open_stdin: false, privileged: false, pid_mode: null,
    resources: { cpu_nanos: null, memory_bytes: null, memory_reservation_bytes: null, shared_memory_bytes: null, devices: [], device_reservations: [], ulimits: {} },
    stop_timeout_secs: null, sysctls: {}, restart: { name: "unless-stopped" }, config_mounts: [],
  },
  placement: { constraints: [] },
  ports: [{ mode: "ingress", hostname: `web.${NAMESPACE}.preview.example.com`, load_balancer_port: 443, container_port: 80, http_protocol: "http" }],
  volumes: [{ reference: "vol-5", source: { kind: "ordinary", name: "vol-5", driver: { name: "local", options: {} }, labels: {} } }],
  mounts: [{ volume: "vol-5", target: "/data", read_only: false, no_copy: false, subpath: null }],
  configs: [],
  pre_deploy: { command: ["/bin/sh", "-c", "migrate"], environment: {}, privileged: null, timeout_millis: null, user: null },
  update: { order: null, monitor_millis: null },
};

const view = (namespace = NAMESPACE): PreviewReviewView =>
  // SAFETY: the fixture is the JSON the Store answers; `previewReviewBody` parses what it reads of it.
  JSON.parse(JSON.stringify({
    environment: { id: "33333333-3333-3333-3333-333333333333", name: "pr-7" },
    namespace,
    rule: { body: canonicalJson(RULE), signatures: [] },
    key: "fork:7",
    commit: COMMIT,
    target: [SPEC],
  })) as PreviewReviewView;

const sha256 = async (text: string) => hex(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));

describe("draftReview", () => {
  it("drafts the review a Server admits, with each Service's template and a 15 minute window", () => {
    const draft = draftReview(view(), CLUSTER, 1_000);
    expect(JSON.parse(draft.body)).toEqual({
      cluster: CLUSTER, commit: COMMIT, environment: "33333333-3333-3333-3333-333333333333", expires_at: 901_000,
      generation: 2, issued_at: 1_000, key: "fork:7", rule: "1".repeat(32), services: [{ digest: TEMPLATE, name: "web" }],
    });
    expect(draft.secrets).toBe(true);
    expect(draft.lines[0]).toMatch(/^Review web at fedcba987654 \(reads DATABASE_URL from the parent/);
    expect(draft.lines.slice(1).every((line) => line.startsWith("  "))).toBe(true);
  });

  it("says why a preview can't be reviewed", () => {
    expect(() => draftReview(view(), "Cg".repeat(21) + "o=", 1_000)).toThrow(ReviewRefused);
    expect(() => draftReview(view("pv-1111111111111111-000000000000"), CLUSTER, 1_000)).toThrow(/namespace the preview rule gives it/);
  });
});

describe("answerReview", () => {
  afterEach(() => void vi.unstubAllGlobals());

  it("answers the review's own challenge with a passkey prompt", async () => {
    const bytes = (text: string) => new TextEncoder().encode(text).buffer;
    const get = vi.fn(async (_options: CredentialRequestOptions) => ({
      rawId: bytes("credential"),
      response: { authenticatorData: bytes("data"), clientDataJSON: bytes("{}"), signature: bytes("signature") },
    }));
    vi.stubGlobal("location", { hostname: "localhost" });
    vi.stubGlobal("navigator", { credentials: { get } });
    const draft = draftReview(view(), CLUSTER, 1_000);
    expect(await answerReview(draft)).toEqual({
      passkey: {
        body: draft.body,
        assertion: { credential_id: "Y3JlZGVudGlhbA", authenticator_data: "ZGF0YQ", client_data_json: "e30", signature: "c2lnbmF0dXJl" },
      },
    });
    const challenge = get.mock.calls[0]?.[0].publicKey?.challenge;
    expect(hex(challenge as Uint8Array)).toBe(await sha256(`ployz-authority/v1\nworkflow\n${draft.body}`));
  });

  it("shows a passkey that didn't answer", async () => {
    vi.stubGlobal("location", { hostname: "localhost" });
    vi.stubGlobal("navigator", { credentials: { get: async () => null } });
    await expect(answerReview(draftReview(view(), CLUSTER, 1_000))).rejects.toThrow("Your passkey didn't answer. Try again.");
  });
});
