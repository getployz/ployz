import { describe, expect, it } from "vitest";
import { CODE, INSTALLATION, REPOSITORY, USER_TOKEN, harness, json, secretsIn } from "./test/harness.js";

describe("audit", () => {
  it("the scan the harness runs over every audit line finds each marker", () => {
    expect(secretsIn(`{"x":"ghs_abc"}`)).toEqual(["ghs_"]);
    expect(secretsIn(USER_TOKEN)).toEqual(["ghu_"]);
    expect(secretsIn("-----BEGIN PRIVATE KEY-----")).toEqual(["-----BEGIN"]);
    expect(secretsIn(`code=${CODE}`)).toEqual([CODE]);
  });

  it("writes one line per request, with what was asked, granted and refused", async () => {
    const minter = harness({ "POST /login/oauth/access_token": () => json({ access_token: USER_TOKEN }), "GET /user": () => json({}, 500) });
    await minter.call("/v1/token", { installation_id: INSTALLATION, repository_ids: [REPOSITORY], permissions: { contents: "read" } });
    await minter.call("/v1/token", { installation_id: INSTALLATION, permissions: { administration: "write" } });
    await minter.call("/v1/connect", { code: CODE });
    expect(minter.audit).toEqual([
      expect.objectContaining({
        caller: "test", route: "/v1/token", outcome: "granted", installation_id: INSTALLATION, repository_ids: [REPOSITORY],
        permissions_asked: { contents: "read" }, permissions_granted: { metadata: "read", contents: "read" }, token_expires_at: expect.any(String),
      }),
      expect.objectContaining({ route: "/v1/token", outcome: "refused", reason: "permission administration is outside the read set", permissions_asked: { administration: "write" } }),
      expect.objectContaining({ route: "/v1/connect", outcome: "failed", reason: "github read_user", github_status: 500 }),
    ]);
    for (const line of minter.audit) {
      expect(line.time).toMatch(/^\d{4}-\d\d-\d\dT/);
      expect(secretsIn(JSON.stringify(line))).toEqual([]);
    }
  });
});
