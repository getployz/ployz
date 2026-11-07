import type { TrustedPasskey } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { approvalOf } from "#/modules/passkeys/passkeys.server";

const trusted = (code: string, approvedBy: TrustedPasskey["approved_by"]) => ({
  code,
  trusted: { approved_by: approvedBy } as TrustedPasskey,
});

describe("approvalOf", () => {
  it("says a passkey Cloud holds is not approved until a Server lists it", () => {
    expect(approvalOf([trusted("bbbbbbbbbb", { by: "server" })], "aaaaaaaaaa", "web-1")).toBeNull();
  });

  it("names the Device or the Server the Server reports as the approver", () => {
    expect(approvalOf([trusted("aaaaaaaaaa", { by: "device", code: "dddddddddd" })], "aaaaaaaaaa", "web-1"))
      .toEqual({ by: "device", code: "dddddddddd", server: "web-1" });
    expect(approvalOf([trusted("aaaaaaaaaa", { by: "server" })], "aaaaaaaaaa", "web-1")).toEqual({ by: "server", server: "web-1" });
  });
});
