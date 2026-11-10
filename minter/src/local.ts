import { createHash, timingSafeEqual } from "node:crypto";
import { readFileSync } from "node:fs";
import { createServer, type IncomingMessage, type Server } from "node:http";
import { pathToFileURL } from "node:url";
import { stdoutAudit } from "./audit.js";
import { Github } from "./github.js";
import { parseBody } from "./lambda.js";
import { CANONICAL_DIGESTS } from "./policy.js";
import { createMinter } from "./routes.js";
import { PemSigner } from "./signer.js";

const MAX_BODY = 1 << 20;

/**
 * The minter as a loopback process for lanes and tests. A bearer secret from a file stands in for IAM. It signs with
 * a PEM file, which the Lambda entry refuses.
 */
export function localServer(handle: ReturnType<typeof createMinter>, bearer: string): Server {
  const expected = createHash("sha256").update(bearer).digest();
  const authenticated = (header: string | undefined) => {
    const given = header?.startsWith("Bearer ") ? header.slice("Bearer ".length) : "";
    return timingSafeEqual(createHash("sha256").update(given).digest(), expected);
  };
  return createServer(async (request, response) => {
    const raw = await readBody(request);
    const result = await handle({
      method: request.method ?? "",
      path: new URL(request.url ?? "/", "http://localhost").pathname,
      caller: authenticated(request.headers.authorization) ? "local-bearer" : null,
      body: raw === null ? undefined : parseBody(raw),
    });
    response.writeHead(result.status, { "Content-Type": "application/json" }).end(JSON.stringify(result.body));
  });
}

async function readBody(request: IncomingMessage): Promise<string | null> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of request) {
    size += (chunk as Buffer).length;
    if (size <= MAX_BODY) chunks.push(chunk as Buffer);
  }
  return size > MAX_BODY ? null : Buffer.concat(chunks).toString("utf8");
}

function env(name: string, fallback?: string): string {
  const value = process.env[name] ?? fallback;
  if (!value) throw new Error(`${name} is not set.`);
  return value;
}

function secretFile(name: string): string {
  return readFileSync(env(name), "utf8").trim();
}

/** A lane's own canonical set, as `{ "digests": [...] }`, for a lab builder fork the deployed set never names. */
function canonical(): ReadonlySet<string> {
  const path = process.env["MINTER_CANONICAL_FILE"];
  if (!path) return CANONICAL_DIGESTS;
  const parsed: unknown = JSON.parse(readFileSync(path, "utf8"));
  const digests = typeof parsed === "object" && parsed !== null ? (parsed as { digests?: unknown }).digests : undefined;
  if (!Array.isArray(digests) || !digests.every((digest) => typeof digest === "string" && /^[0-9a-f]{64}$/.test(digest))) {
    throw new Error("MINTER_CANONICAL_FILE must hold { \"digests\": [<sha256 hex>, ...] }.");
  }
  return new Set(digests);
}

if (process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const appId = env("MINTER_APP_ID");
  const github = new Github(PemSigner.fromFile(env("MINTER_APP_KEY_FILE")), appId, {
    api: env("MINTER_GITHUB_API", "https://api.github.com"),
    web: env("MINTER_GITHUB_WEB", "https://github.com"),
  });
  const handle = createMinter({
    github,
    audit: stdoutAudit,
    config: {
      appId,
      clientId: env("MINTER_APP_CLIENT_ID"),
      clientSecret: secretFile("MINTER_APP_CLIENT_SECRET_FILE"),
      cloud: new URL(env("MINTER_CLOUD_ORIGIN")).origin,
      canonical: canonical(),
    },
  });
  const port = Number(env("MINTER_PORT", "8787"));
  localServer(handle, secretFile("MINTER_BEARER_FILE")).listen(port, "127.0.0.1", () => {
    process.stderr.write(`minter listening on 127.0.0.1:${port}\n`);
  });
}
