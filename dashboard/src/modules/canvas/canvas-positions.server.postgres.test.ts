import { Effect } from "effect";
import { afterAll, beforeAll, expect, it } from "vitest";
import { user } from "#/modules/identity/tables";
import { member, organization } from "#/modules/organization/tables";
import { SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import type { Database } from "#/server/database.server";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import { placeCanvasNodes, updateCanvasPosition } from "./canvas-positions.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000ca01";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000ca02";
const WEB = "00000000-0000-4000-8000-00000000ca03";
const API = "00000000-0000-4000-8000-00000000ca04";
const SLUG = "canvas-places";

let harness: PostgresTestHarness;
let owner: string;

const run = <A, E>(effect: Effect.Effect<A, E, Database | SecretEncryption>) =>
  harness.runEffect(effect.pipe(Effect.provideService(SupportStaff, supportStaffOf([]))));

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const [row] = await harness.db.insert(user).values({ email: "owner@canvas.test", name: "Owner" }).returning({ id: user.id });
  owner = row?.id ?? "";
  await harness.db.insert(organization).values({ id: ORGANIZATION, name: "Canvas", slug: SLUG });
  await harness.db.insert(member).values({ userId: owner, organizationId: ORGANIZATION, role: "owner" });
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

it("places a node once and never over a place a person dragged it to", async () => {
  await run(updateCanvasPosition({ userId: owner }, { organizationSlug: SLUG, environmentId: ENVIRONMENT, resourceType: "service", resourceId: WEB, x: 900, y: -300 }));
  const place = (x: number) => run(placeCanvasNodes({ userId: owner }, { organizationSlug: SLUG, environmentId: ENVIRONMENT, nodes: [
    { resourceType: "service", resourceId: WEB, x, y: 0 },
    { resourceType: "service", resourceId: API, x: x + 336, y: 0 },
    { resourceType: "live", resourceId: `${ENVIRONMENT}/redis`, x: x + 672, y: 0 },
  ] }));
  const first = await place(0);
  const places = (rows: typeof first.data) => Object.fromEntries(rows.map((row) => [row.resourceId, [row.x, row.y]]));
  expect(places(first.data)).toEqual({ [WEB]: [900, -300], [API]: [336, 0], [`${ENVIRONMENT}/redis`]: [672, 0] });
  // A second tab placing the same nodes elsewhere is answered with the places already kept.
  expect(places((await place(48)).data)).toEqual(places(first.data));
});
