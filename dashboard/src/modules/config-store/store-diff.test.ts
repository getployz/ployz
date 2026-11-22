import type { DiffView, JsonValue, ServiceSettingChange } from "@ployz/sdk";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { withoutSealedBefore } from "#/modules/config-store/store-diff";

it("keeps no plain value of a variable the reviewed change seals, as the Store keeps none once that change saves", () => {
  const change = (path: string, before: JsonValue, after: JsonValue): ServiceSettingChange =>
    ({ path, kind: "update", before, after, canRestore: true, row: null });
  const node = { type: "service", id: "svc", name: "web", row: "svc", lifecycle: "update", comparison: "head", data: null, restarts: [] } as const;
  const diff = asTestDouble<DiffView>()({
    changes: [{ ...node, settings: [
      change("web.env.TOKEN", "fake-plain", { secret: true }),
      change("web.env.GONE", "fake-gone", { secret: false }),
      change("web.env.OPEN", "fake-open", "fake-open-2"),
      change("web.image", "nginx:1", "nginx:2"),
    ] }],
    draft_changes: [{ ...node, settings: [change("web.env.TOKEN", "fake-plain", { secret: true })] }],
  });
  const sealed = withoutSealedBefore(diff);
  expect(JSON.stringify(sealed)).not.toContain("fake-plain");
  expect(JSON.stringify(sealed)).not.toContain("fake-gone");
  expect(sealed.changes[0]?.settings.map((row) => row.before)).toEqual([{ secret: true }, { secret: true }, "fake-open", "nginx:1"]);
  expect(sealed.draft_changes?.[0]?.settings[0]?.before).toEqual({ secret: true });
  // A review Cloud kept with only its version and Environment reads as it was.
  const bare = asTestDouble<DiffView>()({ version: "7", environment: { id: "e", project: "shop", name: "production" } });
  expect(withoutSealedBefore(bare)).toEqual(bare);
});
