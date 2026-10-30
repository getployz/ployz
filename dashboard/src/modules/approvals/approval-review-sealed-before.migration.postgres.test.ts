import { readFile } from "node:fs/promises";
import type { DiffView, JsonValue, NodeChange, RowId, ServiceSettingChange } from "@ployz/sdk";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { type ApprovalReview, withoutSealedBefore } from "#/modules/approvals/approvals";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000003651";
const MARKER = "fake-plain-marker-3651";
const scrub = new URL("../../../drizzle/20261030150000_approval_review_sealed_before/migration.sql", import.meta.url);

const setting = (path: string, row: string | null, before: JsonValue, after: JsonValue): ServiceSettingChange =>
  ({ path, kind: "update", before, after, canRestore: false, row: row as RowId | null });
const node = (settings: ServiceSettingChange[]): NodeChange =>
  ({ type: "service", id: "api", name: "api", row: "api:node" as RowId, lifecycle: "update", comparison: "head", data: null, restarts: [], settings });
const sealed = { secret: true };
const environment = { id: "env-1", name: "production", project: "shop" };
const diffOf = (fields: Partial<DiffView>) => asTestDouble<DiffView>()({ environment, version: "v1", ...fields });
const reviewOf = (diff: DiffView): ApprovalReview => ({ effects: [{ kind: "removes_service", node: "web", path: "web", name: "web" }], diff });

const preFix = diffOf({
  changes: [node([
    setting("api.env.TOKEN", "api:variables.TOKEN", MARKER, sealed),
    setting("api.env.OTHER", "api:variables.OTHER", "fake-unsealed-value", "fake-still-plain"),
    setting("api.env.NEW", "api:variables.NEW", null, sealed),
    setting("api.env.KEPT", "api:variables.KEPT", sealed, sealed),
    setting("api.env.TOKEN.description", "", "fake description", sealed),
    setting("configs.@app.files.a", "app:files.a", "fake file text", sealed),
  ]), node([setting("web.env.PATH_ONLY", null, `${MARKER}-path`, sealed)])],
  draft_changes: [node([setting("api.variables.DRAFT", "", `${MARKER}-draft`, sealed)])],
});

describe("the approval review sealed-before scrub", () => {
  let harness: PostgresTestHarness;

  const insert = async (review: ApprovalReview) => {
    const { rows } = await harness.pool.query<{ id: string }>(
      `insert into operation_approvals (organization_id, subject, credential_kind, credential_id, command, review, digest)
       values ($1, 'env-1', 'token', 'tok-1', 'publish', $2, gen_random_uuid()::text) returning id`,
      [organizationId, JSON.stringify(review)],
    );
    return rows.map((row) => row.id).join();
  };
  const stored = async (id: string) =>
    (await harness.pool.query<{ review: ApprovalReview; version: string }>(`select review, xmin::text as version from operation_approvals where id = $1`, [id])).rows;
  const run = async () => harness.pool.query(await readFile(scrub, "utf8"));

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });
  beforeEach(async () => {
    await harness.pool.query(`truncate table organization cascade; insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');`);
  });

  it("stores a pre-0.3.11 review as Cloud writes it now, and holds no plain value it sealed", async () => {
    const id = await insert(reviewOf(preFix));

    await run();

    const [row] = await stored(id);
    expect(row?.review).toEqual(reviewOf(withoutSealedBefore(preFix)));
    const holding = await harness.pool.query(`select id from operation_approvals where strpos(review::text, $1) > 0`, [MARKER]);
    expect(holding.rows).toEqual([]);
    for (const plain of ["fake-unsealed-value", "fake-still-plain", "fake description", "fake file text"]) expect(JSON.stringify(row?.review)).toContain(plain);
  });

  it("writes no row already in that shape, and changes nothing run again", async () => {
    const reviews: ApprovalReview[] = [
      reviewOf(withoutSealedBefore(preFix)),
      reviewOf(diffOf({})),
      reviewOf(diffOf({ changes: [], draft_changes: [] })),
      { effects: [], operation: { verb: "drain", name: "fra-1", preview: { before: MARKER, after: sealed } } },
    ];
    const ids = await Promise.all(reviews.map(insert));
    const dirty = await insert(reviewOf(preFix));
    const before = await Promise.all(ids.map(stored));

    await run();
    const scrubbed = await stored(dirty);
    await run();

    expect(await Promise.all(ids.map(stored))).toEqual(before);
    expect(before.map(([row]) => row?.review)).toEqual(reviews);
    expect(await stored(dirty)).toEqual(scrubbed);
  });
});
