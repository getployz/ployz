import { randomUUID } from "node:crypto";
import { expect, it } from "vitest";
import { serverAccessLabel } from "#/modules/machines/server-access.server";

it("labels a credential's Server access outside the Device namespace", () => {
  for (const id of [randomUUID(), "device-0000", "", "-".repeat(40)]) {
    expect(serverAccessLabel(id)).toMatch(/^cli-/);
  }
});
