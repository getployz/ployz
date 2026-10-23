import { randomUUID } from "node:crypto";
import { expect, it } from "vitest";
import { serverAccessLabel } from "#/modules/machines/server-access.server";

// A Machine refuses a Cloud-minted slot in the Device namespace, so Cloud's labels must never land there.
it("labels a credential's Server access outside the Device namespace", () => {
  for (const id of [randomUUID(), "device-0000", "", "-".repeat(40)]) {
    expect(serverAccessLabel(id)).toMatch(/^cli-/);
  }
});
