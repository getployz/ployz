import { afterEach, expect, it, vi } from "vitest";
import * as passkey from "./passkey-statement";
import type { Review } from "./approval-prompt";
import { approveTeardown, departure, TEARDOWN_CAPABILITY, type TeardownAsk, TeardownRefused } from "./teardown-approval";
import { removalTeardown } from "#/modules/config-store/store-workspace";

const CLUSTER = "NdiV1QY9P3ZyoC82YA/FaBs0zlWTG/CXU1tLfcS0CDg=";
const OTHER = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";
const A = "a".repeat(32);
const B = "b".repeat(32);
const C = "c".repeat(32);
const ASSERTION = { credential_id: "AQID", authenticator_data: "AQID", client_data_json: "AQID", signature: "AQID" };

const ENFORCED = "ployz.machine.enforced.v1";

const server = (id: string, name: string, cluster = CLUSTER, capabilities = [ENFORCED, TEARDOWN_CAPABILITY]) =>
  ({ id, name, sealing: { kind: "held", public_key: cluster }, capabilities });

const ask = (over: Partial<TeardownAsk> = {}): TeardownAsk => ({
  machines: [server(A, "web-1"), server(B, "web-2"), server(C, "elsewhere", OTHER)],
  near: [A],
  scope: (machines) => ({ kind: "namespace", namespaces: ["shop-production"], machines, volumes: ["shop-production_vol-1"] }),
  volumes: { "shop-production_vol-1": "data" },
  title: "Delete shop/production?",
  action: "Delete",
  ...over,
});

afterEach(() => {
  vi.restoreAllMocks();
});

it("refuses before asking while a Server of the Cluster runs Ployz 0.3, naming the upgrade", async () => {
  const review = vi.fn(async () => true);
  const machines = [server(A, "web-1"), server(B, "web-2", CLUSTER, []), server(C, "elsewhere", OTHER, [])];

  await expect(approveTeardown(ask({ machines }), review)).rejects.toEqual(new TeardownRefused("web-2 runs Ployz 0.3. Upgrade it to deploy. Nothing deleted."));
  expect(review).not.toHaveBeenCalled();
});

it("shows every term of the body it drafts, then has the passkey answer exactly that body", async () => {
  const now = Date.now();
  const challenge = vi.spyOn(passkey, "statementChallenge");
  vi.spyOn(passkey, "passkeyAnswer").mockResolvedValue(ASSERTION);
  let shown: Review | undefined;

  const approved = await approveTeardown(ask(), async (review) => {
    shown = review;
    return true;
  }, now);

  // The Cluster web-1 holds the key of, every Server of it and none of another, sorted by ployz-core.
  const body = `{"cluster":"${CLUSTER}","expires_at":${now + 900_000},"issued_at":${now},"scope":{"kind":"namespace","machines":["${A}","${B}"],`
    + `"namespaces":["shop-production"],"volumes":["shop-production_vol-1"]},"signer":"passkey"}`;
  expect(approved).toEqual({ passkey: { body, assertion: ASSERTION } });
  expect(challenge).toHaveBeenCalledExactlyOnceWith(body, "workflow");
  expect(shown?.lines).toEqual([
    "Approved: stop and remove every service of shop-production on web-1, web-2.",
    "Approved: delete volume data (shop-production_vol-1) there. Its data is gone for good.",
    `  cluster "${CLUSTER}"`,
    "  signer your passkey",
    `  valid ${new Date(now).toISOString().replace(/\.\d{3}Z$/, "Z")} to ${new Date(now + 900_000).toISOString().replace(/\.\d{3}Z$/, "Z")}`,
  ]);
});

it("signs nothing when the person cancels the review or the passkey doesn't answer", async () => {
  const answer = vi.spyOn(passkey, "passkeyAnswer");

  await expect(approveTeardown(ask(), async () => false)).rejects.toEqual(new TeardownRefused("You didn't approve it. Nothing deleted."));
  expect(answer).not.toHaveBeenCalled();

  answer.mockRejectedValue(Object.assign(new Error("denied"), { name: "NotAllowedError" }));
  await expect(approveTeardown(ask(), async () => true)).rejects.toEqual(new TeardownRefused("Your passkey didn't answer. Nothing deleted."));
});

it("refuses before asking when a Server can't take a teardown yet", async () => {
  const review = vi.fn(async () => true);
  const machines = [server(A, "web-1", CLUSTER, [ENFORCED])];

  await expect(approveTeardown(ask({ machines }), review)).rejects.toBeInstanceOf(TeardownRefused);
  expect(review).not.toHaveBeenCalled();
});

it("names the departing Server, and every other Server of the Cluster as forgetting it", async () => {
  vi.spyOn(passkey, "passkeyAnswer").mockResolvedValue(ASSERTION);
  let shown: readonly string[] = [];

  const approved = await approveTeardown(ask({ scope: departure(B, ["shop-production_vol-1"]), near: [B] }), async (review) => {
    shown = review.lines;
    return true;
  });

  expect(JSON.parse(approved?.passkey.body ?? "{}").scope).toEqual({ kind: "server", leaving: B, forgetting: [A], volumes: ["shop-production_vol-1"] });
  expect(shown.slice(0, 3)).toEqual([
    "Approved: remove web-2 from the Cluster. Its services stop there.",
    "Approved: web-1 forget web-2.",
    "The Cluster can no longer reach its copies of data (shop-production_vol-1).",
  ]);
});

it("names each Namespace a deletion takes off, and only the Volumes the person accepted losing", () => {
  const removed = [
    { name: "production", namespace: "shop-production", volumes: [{ name: "data", docker_volume: "shop-production_vol-1" }, { name: "cache", docker_volume: "shop-production_vol-2" }] },
    { name: "staging", namespace: "shop-staging", volumes: [{ name: "data", docker_volume: "shop-staging_vol-3" }] },
  ];

  expect(removalTeardown(removed, { accept: { production: ["data"] }, version: null, everyCopy: { staging: ["data"] } })).toEqual({
    namespaces: ["shop-production", "shop-staging"],
    volumes: { "shop-production_vol-1": "data", "shop-staging_vol-3": "data" },
  });
});
