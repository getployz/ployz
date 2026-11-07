import type { TrustedPasskey } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { approvalOf } from "#/modules/passkeys/passkeys.server";

const trusted = (code: string, approvedBy: TrustedPasskey["approved_by"]) => ({
  code,
  trusted: { approved_by: approvedBy } as TrustedPasskey,
});

describe("approvalOf", () => {
  it("says a passkey Cloud holds is not approved until a Server lists it", () => {
    expect(approvalOf([trusted("bbbbbbbbbb", { by: "server" })], "aaaaaaaaaa")).toBeNull();
  });

  it("names the Device or the Server the Server reports as the approver", () => {
    expect(approvalOf([trusted("aaaaaaaaaa", { by: "device", code: "dddddddddd" })], "aaaaaaaaaa"))
      .toEqual({ by: "device", code: "dddddddddd" });
    expect(approvalOf([trusted("aaaaaaaaaa", { by: "server" })], "aaaaaaaaaa")).toEqual({ by: "server" });
  });

  it("claims no terminal approval for a call routed to the Server or an approver it can't name", () => {
    expect(approvalOf([trusted("aaaaaaaaaa", { by: "routed" })], "aaaaaaaaaa")).toEqual({ by: "trusted" });
    expect(approvalOf([trusted("aaaaaaaaaa", { by: "unknown" })], "aaaaaaaaaa")).toEqual({ by: "trusted" });
  });
});
