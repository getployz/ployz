import { generateKeyPairSync, randomBytes } from "node:crypto";
import { afterAll, expect } from "vitest";
import type { AuditLine } from "../audit.js";
import { Github } from "../github.js";
import { workflowDigest } from "../policy.js";
import { createMinter, type MinterConfig } from "../routes.js";
import { PemSigner } from "../signer.js";

export const APP_ID = "4242";
export const INSTALLATION = 11;
export const REPOSITORY = 42;
export const HEAD = "a".repeat(40);
export const BUILDER = "b".repeat(40);
export const CLIENT_SECRET = `stub-client-secret-${randomBytes(8).toString("hex")}`;
export const CODE = `stub-code-${randomBytes(8).toString("hex")}`;
export const USER_TOKEN = `ghu_${randomBytes(12).toString("hex")}`;
export const CLOUD = "https://cloud.ployz.test";
export const API = "https://api.github.test";

export const keys = generateKeyPairSync("rsa", { modulusLength: 2048 });
const PEM = keys.privateKey.export({ type: "pkcs8", format: "pem" }).toString();

/** A stand-in for a canonical template: the builder pinned by commit, and a read-only `GITHUB_TOKEN`. */
export const CANONICAL_WORKFLOW = `name: Ployz build
on:
  workflow_dispatch:
    inputs:
      build: { required: true, type: string }
      cloud: { required: true, type: string }
      runner: { required: false, type: string, default: ubuntu-latest }
      commit: { required: false, type: string }
permissions:
  contents: read
  id-token: write
jobs:
  build:
    runs-on: \${{ inputs.runner }}
    steps:
      - uses: getployz/build@${BUILDER}
        with:
          build: \${{ inputs.build }}
          cloud: \${{ inputs.cloud }}
          commit: \${{ inputs.commit }}
`;

export interface Call {
  method: string;
  path: string;
  query: URLSearchParams;
  headers: Headers;
  body: unknown;
}

export type Reply = Response | { status?: number; body?: unknown };
export type StubRoutes = Record<string, (call: Call) => Reply | Promise<Reply>>;

export const json = (body: unknown, status = 200) => ({ status, body });
export const raw = (text: string) => new Response(text, { status: 200 });
export const notFound = () => json({ message: "Not Found" }, 404);

/** GitHub's token endpoint: grants what was asked, plus the `metadata: read` GitHub adds to every token. */
function mintToken(call: Call): Reply {
  const asked = (call.body as { permissions?: Record<string, string> } | undefined)?.permissions ?? { administration: "write", contents: "write" };
  return json({
    token: `ghs_${randomBytes(12).toString("hex")}`,
    expires_at: "2099-01-01T00:00:00Z",
    permissions: { metadata: "read", ...asked },
  }, 201);
}

const lines: AuditLine[] = [];
const responses: unknown[] = [];

/** Markers that must never reach an audit line or a response that is not a token response. */
const SECRETS = ["ghu_", "-----BEGIN", CODE, CLIENT_SECRET];

export function secretsIn(text: string, markers: readonly string[] = ["ghs_", ...SECRETS]): string[] {
  return markers.filter((marker) => text.includes(marker));
}

afterAll(() => {
  for (const line of lines) expect(secretsIn(JSON.stringify(line)), JSON.stringify(line)).toEqual([]);
  for (const response of responses) expect(secretsIn(JSON.stringify(response), SECRETS)).toEqual([]);
});

/** A minter wired to a stub GitHub. Every audit line and response it writes is scanned for secrets when the file ends. */
export function harness(routes: StubRoutes = {}, config: Partial<MinterConfig> = {}, now?: () => number) {
  const calls: Call[] = [];
  const audit: AuditLine[] = [];
  const fetch: typeof globalThis.fetch = async (input, init) => {
    const url = new URL(String(input));
    const call: Call = {
      method: init?.method ?? "GET",
      path: url.pathname,
      query: url.searchParams,
      headers: new Headers(init?.headers),
      body: typeof init?.body === "string" ? JSON.parse(init.body) : undefined,
    };
    calls.push(call);
    const key = `${call.method} ${call.path}`;
    const handler = routes[key] ?? (/^POST \/app\/installations\/\d+\/access_tokens$/.test(key) ? mintToken : undefined);
    const reply = await (handler ? handler(call) : notFound());
    return reply instanceof Response ? reply : new Response(reply.body === undefined ? null : JSON.stringify(reply.body), { status: reply.status ?? 200 });
  };
  const github = new Github(new PemSigner(PEM), APP_ID, { api: API, web: "https://github.test" }, fetch, now);
  const handle = createMinter({
    github,
    ...(now === undefined ? {} : { now }),
    audit: (line) => {
      audit.push(line);
      lines.push(line);
    },
    config: {
      appId: APP_ID,
      clientId: "Iv1.stub",
      clientSecret: CLIENT_SECRET,
      cloud: CLOUD,
      canonical: new Set([workflowDigest(Buffer.from(CANONICAL_WORKFLOW))]),
      ...config,
    },
  });
  const call = async (path: string, body: unknown) => {
    const response = await handle({ method: "POST", path, caller: "test", body });
    if (path !== "/v1/token") responses.push(response.body);
    return response;
  };
  return { call, handle, calls, audit };
}

/** The calls the minter made to GitHub with a body that asked for a token. */
export function tokenRequests(calls: readonly Call[]) {
  return calls.filter((call) => call.method === "POST" && call.path.endsWith("/access_tokens"));
}
