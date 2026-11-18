import { describe, expect, it } from "vitest";
import { answeredBy } from "./approvals.queries";

describe("answeredBy", () => {
  const theirs = { protected: false, yours: true };
  const anOwners = { protected: true, yours: true };

  it("is the member while any Approval the chat asked for is theirs to answer", () => {
    expect(answeredBy([anOwners, theirs])).toBe("you");
  });

  it("is an owner or admin once every Approval the chat asked for is out of the member's hands", () => {
    expect(answeredBy([anOwners])).toBe("others");
  });
});
