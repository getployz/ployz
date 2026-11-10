import { verify } from "node:crypto";
import { describe, expect, it } from "vitest";
import { Github } from "./github.js";
import { createLambdaHandler } from "./lambda.js";
import { KmsSigner, PemSigner } from "./signer.js";
import { APP_ID, keys } from "./test/harness.js";

const config = { appId: APP_ID, clientId: "Iv1.stub", clientSecret: "unused", cloud: "https://cloud.ployz.test" };

describe("signers", () => {
  it("a PemSigner JWT verifies with the App's public key", async () => {
    const pem = keys.privateKey.export({ type: "pkcs8", format: "pem" }).toString();
    const github = new Github(new PemSigner(pem), APP_ID, { api: "https://api.github.test", web: "https://github.test" }, fetch, () => 1_800_000_000_000);
    const [header, payload, signature] = (await github.appJwt()).split(".");
    expect(JSON.parse(Buffer.from(header ?? "", "base64url").toString())).toEqual({ alg: "RS256", typ: "JWT" });
    expect(JSON.parse(Buffer.from(payload ?? "", "base64url").toString())).toEqual({ iss: APP_ID, iat: 1_800_000_000 - 60, exp: 1_800_000_000 + 540 });
    const valid = verify("sha256", Buffer.from(`${header}.${payload}`), keys.publicKey, Buffer.from(signature ?? "", "base64url"));
    expect(valid).toBe(true);
  });

  it("the Lambda entry refuses a PemSigner", () => {
    const pem = keys.privateKey.export({ type: "pkcs8", format: "pem" }).toString();
    expect(() => createLambdaHandler({ signer: new PemSigner(pem), config })).toThrow("signs only with KMS");
  });

  it("the Lambda entry takes a KmsSigner and refuses a request IAM didn't authenticate", async () => {
    const lines: unknown[] = [];
    const handler = createLambdaHandler({ signer: new KmsSigner("alias/unused"), config, audit: (line) => lines.push(line) });
    const result = await handler({ rawPath: "/v1/token", body: "{\"installation_id\":1}", requestContext: { http: { method: "POST" } } });
    expect(result.statusCode).toBe(403);
    expect(lines).toEqual([expect.objectContaining({ caller: "unauthenticated", outcome: "refused" })]);
  });
});
