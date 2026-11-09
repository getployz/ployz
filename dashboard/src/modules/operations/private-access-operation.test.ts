import { describe, expect, it } from "vitest";
import { approvalBadge, approvalSubject, asksForAuthKey, type ApprovalReview } from "#/modules/approvals/approvals";
import { OPERATIONS } from "#/modules/operations/operations";
import { privateAccessUse } from "#/modules/private-access/private-access.commands.server";
import { privateAccessAsked } from "#/modules/private-access/private-access.server";

const reviewOf = (to: "on" | "off"): ApprovalReview => {
  const asked = privateAccessAsked("org-1", to);
  return { effects: asked.effects, operation: { verb: asked.verb, name: asked.name, preview: asked.preview } };
};

describe("setPrivateAccess gating", () => {
  it("asks a human from the CLI or an agent, and only an operator may ask", () => {
    const operation = OPERATIONS["private_access.use"];
    expect(operation.gate).toBe("approval");
    expect(operation.requires).toBe("servers:operate");
    expect(privateAccessUse.gate).toBe("ask");
    expect(privateAccessUse.removes).toBe(false);
  });

  it("asks even though it destroys nothing, on the whole Cluster", () => {
    for (const to of ["on", "off"] as const) {
      expect(privateAccessAsked("org-1", to)).toMatchObject({ subject: "cluster:org-1", verb: "set_private_access", effects: [], alwaysAsks: true });
    }
  });

  it("takes no auth key from the CLI or an agent", () => {
    expect(privateAccessUse.input.fields).not.toHaveProperty("authKey");
    expect(privateAccessUse.input.fields).not.toHaveProperty("auth_key");
    expect(Object.keys(privateAccessUse.input.fields)).toEqual(["to"]);
  });

  it("asks the approver for the key only when it turns on", () => {
    expect(asksForAuthKey(reviewOf("on"))).toBe(true);
    expect(asksForAuthKey(reviewOf("off"))).toBe(false);
    expect(approvalSubject("set_private_access", reviewOf("on"))).toEqual({ verb: "Turn private access", name: "on", project: null });
    expect(approvalBadge("set_private_access", reviewOf("on"))).toBe("Joins a tailnet");
    expect(approvalBadge("set_private_access", reviewOf("off"))).toBe("Removes routers");
  });
});
