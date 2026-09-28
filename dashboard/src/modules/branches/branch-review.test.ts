import { describe, expect, it } from "vitest";
import { branchChanges, parseServiceConfig } from "@ployz/sdk/config";
import { emptyEnvironmentIntent, type SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { branchReview, liveUpdates, presentRow, usedLive, type BranchReviewInput } from "./branch-review";

const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const [WEB, DB, SEARCH] = [id(1), id(2), id(3)];
const names = new Map([[WEB, "web"], [DB, "postgres"], [SEARCH, "search"]]);
const nameOf = (lineage: string) => names.get(lineage) ?? lineage;

function service(n: number, lineageId: string, slug: string, image = `${slug}:1`) {
  const { env: _env, mounts: _mounts, ...config } = parseServiceConfig({
    version: 2, source: { version: 1, type: "image", image, credentials: { type: "none" } },
    healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns: slug,
  });
  return { id: id(10 + n), lineageId, slug, config, variables: [], volumeAttachments: [] } as SavedEnvironmentIntent["services"][number];
}

/** production: web uses postgres live. */
function production(): SavedEnvironmentIntent {
  const web = service(1, WEB, "web");
  web.variables = [{ id: id(20), key: "DB", description: null, exported: false, valueFingerprint: "fp",
    value: { kind: "template", parts: [{ kind: "ref", owner: { scope: "service", lineageId: DB }, key: "URL" }] } }];
  return { version: 1, environmentSlug: "shop-production", services: [web, service(2, DB, "postgres")], volumes: [] };
}

/** fix-web: a Branch of production with its own web, using production's postgres live. */
function branchOf(parent: SavedEnvironmentIntent) {
  const create = { base: null, from: parent, into: emptyEnvironmentIntent("shop-fix-web"), provided: [DB],
    hostnames: { from: "", into: "-fix-web" }, fromKept: false };
  const created = branchChanges({ ...create, picks: [{ key: `${WEB}:node` }] });
  if (!created.base) throw new Error("Creating returns a base.");
  return { branch: created.next, base: created.base };
}

function input(edit: (sides: { branch: SavedEnvironmentIntent; parent: SavedEnvironmentIntent; parentApplied: SavedEnvironmentIntent }) => void): BranchReviewInput {
  const parent = production();
  const { branch, base } = branchOf(parent);
  const sides = { branch, parent, parentApplied: production() };
  edit(sides);
  return { base, kept: false, ...sides, hostnames: { branch: "-fix-web", parent: "" } };
}

const keys = (rows: { key: string }[]) => rows.map((row) => row.key);
function web(intent: SavedEnvironmentIntent) {
  const node = intent.services.find((candidate) => candidate.lineageId === WEB);
  if (!node) throw new Error("No web.");
  return node;
}
const image = (intent: SavedEnvironmentIntent, tag: string) => {
  const node = web(intent);
  if (node.config.source.type === "image") node.config.source.image = tag;
};
function only<T>(rows: T[]) {
  expect(rows).toHaveLength(1);
  const [row] = rows;
  if (!row) throw new Error("No row.");
  return row;
}

describe("branch review", () => {
  it("shows nothing to save or update right after branching, and postgres as used live", () => {
    const review = branchReview(input(() => {}));
    expect(review.save).toEqual([]);
    expect(review.update).toEqual([]);
    expect(review.differ.map((row) => [row.key, row.role === "differ" && row.why])).toEqual([[`${DB}:node`, "live"]]);
  });

  it("saves the Branch's changes, marking one production also changed", () => {
    const review = branchReview(input(({ branch, parent }) => {
      image(branch, "web:2");
      web(branch).config.replicas = 3;
      image(parent, "web:1.1");
    }));
    const row = only(review.save);
    expect(row.key).toBe(`${WEB}:source.image`);
    expect(row.role === "move" && row.conflict).toBe(true);
    expect(presentRow(row, nameOf)).toMatchObject({ node: "web", label: "Container image", before: "web:1.1", after: "web:2" });
    expect(review.differ.map((differ) => differ.role === "differ" && differ.why)).toEqual(["sizing", "live"]);
  });

  it("lists what production deployed that the Branch lacks, not what it only staged", () => {
    const review = branchReview(input(({ parent, parentApplied }) => {
      parentApplied.services.push(service(3, SEARCH, "search"));
      image(parent, "web:staged-only");
    }));
    expect(keys(review.update)).toEqual([`${SEARCH}:node`]);
    expect(presentRow(only(review.update), nameOf)).toMatchObject({ node: "search", label: "New", after: "" });
  });

  it("has nothing to update before production's first deploy", () => {
    expect(branchReview({ ...input(() => {}), parentApplied: null }).update).toEqual([]);
  });

  it("finds the lineages an Environment uses live", () => {
    expect(usedLive(production())).toEqual([]);
    expect(usedLive(branchOf(production()).branch)).toEqual([DB]);
  });

  it("lists Live Nodes their owner redeployed after the Branch last deployed", () => {
    const at = (day: number) => new Date(Date.UTC(2026, 8, day));
    const deployedAt = new Map([["prod", { [DB]: at(5) }], ["staging", { [DB]: at(3), [SEARCH]: at(9) }]]);
    const updates = (branchDeployedAt: Date | null) => liveUpdates({
      live: [DB, SEARCH], parentId: "staging", branches: [{ environmentId: "staging", parentEnvironmentId: "prod" }], branchDeployedAt, deployedAt,
    });
    // The nearest ancestor that deployed it owns it: staging owns both here.
    expect(updates(at(4))).toEqual([{ lineageId: SEARCH, ownerEnvironmentId: "staging", deployedAt: at(9) }]);
    expect(updates(at(10))).toEqual([]);
    expect(updates(null)).toEqual([]);
  });
});
