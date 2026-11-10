import type { Signer } from "./signer.js";

export const GITHUB_API_VERSION = "2022-11-28";
const TIMEOUT_MS = 10_000;

export interface GithubOrigins {
  api: string;
  web: string;
}

/** A GitHub call that failed. It carries the status and never the response body, which may echo a credential. */
export class GithubError extends Error {
  constructor(readonly operation: string, readonly status: number | null, readonly retryAfter: number | null = null) {
    super(`GitHub ${operation} failed${status === null ? "" : ` (${status})`}`);
  }
}

export type TokenPermissions = Readonly<Record<string, "read" | "write">>;

export interface MintedToken {
  token: string;
  expiresAt: string;
  permissions: Record<string, string>;
}

type Method = "GET" | "POST" | "PATCH";

/** The App's view of GitHub: JWTs from the signer, installation tokens, and the calls routes make with them. */
export class Github {
  constructor(
    private readonly signer: Signer,
    private readonly appId: string,
    private readonly origins: GithubOrigins,
    private readonly fetchImpl: typeof fetch = fetch,
    private readonly now: () => number = Date.now,
  ) {}

  /** An App JWT, issued 60 seconds back for clock drift and expiring 9 minutes on, inside GitHub's 10 minute cap. */
  async appJwt(): Promise<string> {
    const now = Math.floor(this.now() / 1000);
    const header = base64url(JSON.stringify({ alg: "RS256", typ: "JWT" }));
    const payload = base64url(JSON.stringify({ iss: this.appId, iat: now - 60, exp: now + 540 }));
    const signature = await this.signer.sign(Buffer.from(`${header}.${payload}`));
    return `${header}.${payload}.${Buffer.from(signature).toString("base64url")}`;
  }

  /** An installation token. `permissions` is required, so GitHub never falls back to every permission it accepted. */
  async mint(installationId: number, request: { permissions: TokenPermissions; repositoryIds?: readonly number[] }): Promise<MintedToken> {
    const body: Record<string, unknown> = { permissions: request.permissions };
    if (request.repositoryIds !== undefined) body["repository_ids"] = request.repositoryIds;
    const data = await this.json("installation_token", await this.appJwt(), "POST", `/app/installations/${installationId}/access_tokens`, body);
    return {
      token: field(data, "token", "string", "installation_token"),
      expiresAt: field(data, "expires_at", "string", "installation_token"),
      permissions: stringRecord(field(data, "permissions", "object", "installation_token"), "installation_token"),
    };
  }

  async app(operation: string, path: `/${string}`): Promise<unknown> {
    return this.json(operation, await this.appJwt(), "GET", path);
  }

  async json(operation: string, token: string, method: Method, path: `/${string}`, body?: unknown): Promise<unknown> {
    const response = await this.send(operation, token, method, path, body === undefined ? {} : { body });
    return response.status === 204 ? null : readJson(response, operation);
  }

  /** The response when it succeeded or its status is in `allow`, for the calls whose refusals carry meaning. */
  async send(operation: string, token: string, method: Method, path: `/${string}`, options: {
    body?: unknown; accept?: string; allow?: readonly number[];
  } = {}): Promise<Response> {
    const headers: Record<string, string> = {
      Accept: options.accept ?? "application/vnd.github+json",
      Authorization: `Bearer ${token}`,
      "User-Agent": "ployz-minter",
      "X-GitHub-Api-Version": GITHUB_API_VERSION,
    };
    if (options.body !== undefined) headers["Content-Type"] = "application/json";
    const response = await this.request(operation, `${this.origins.api}${path}`, {
      method,
      headers,
      ...(options.body === undefined ? {} : { body: JSON.stringify(options.body) }),
    });
    if (response.ok || options.allow?.includes(response.status)) return response;
    throw await failed(response, operation);
  }

  /** A file's bytes at a ref, or null when the ref holds no such file. */
  async file(token: string, path: `/${string}`): Promise<Uint8Array | null> {
    const response = await this.send("read_file", token, "GET", path, { accept: "application/vnd.github.raw", allow: [404] });
    if (response.status === 404) {
      await response.body?.cancel();
      return null;
    }
    return new Uint8Array(await response.arrayBuffer());
  }

  /** Exchanges a connect code for the person's user token. The caller keeps the token inside its own handler. */
  async exchangeCode(clientId: string, clientSecret: string, code: string): Promise<string> {
    const response = await this.request("exchange_user_code", `${this.origins.web}/login/oauth/access_token`, {
      method: "POST",
      headers: { Accept: "application/json", "Content-Type": "application/json", "User-Agent": "ployz-minter" },
      body: JSON.stringify({ client_id: clientId, client_secret: clientSecret, code }),
    });
    if (!response.ok) throw await failed(response, "exchange_user_code");
    const data = await readJson(response, "exchange_user_code");
    const token = isRecord(data) ? data["access_token"] : undefined;
    if (typeof token !== "string" || token.length === 0) throw new GithubError("exchange_user_code", response.status);
    return token;
  }

  private async request(operation: string, url: string, init: RequestInit): Promise<Response> {
    try {
      return await this.fetchImpl(url, { ...init, redirect: "error", signal: AbortSignal.timeout(TIMEOUT_MS) });
    } catch {
      throw new GithubError(operation, null);
    }
  }
}

async function failed(response: Response, operation: string): Promise<GithubError> {
  await response.body?.cancel();
  const retryAfter = response.headers.get("retry-after");
  return new GithubError(operation, response.status, retryAfter && /^\d+$/.test(retryAfter) ? Number(retryAfter) : null);
}

async function readJson(response: Response, operation: string): Promise<unknown> {
  try {
    return await response.json();
  } catch {
    throw new GithubError(operation, response.status);
  }
}

function base64url(value: string): string {
  return Buffer.from(value).toString("base64url");
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

type Kinds = { string: string; number: number; object: Record<string, unknown>; array: unknown[]; boolean: boolean };

/** One field of a GitHub response, or a `GithubError` naming the operation when GitHub answered another shape. */
export function field<K extends keyof Kinds>(data: unknown, key: string, kind: K, operation: string): Kinds[K] {
  const value = isRecord(data) ? data[key] : undefined;
  const matches = kind === "array" ? Array.isArray(value) : kind === "object" ? isRecord(value) : typeof value === kind;
  if (!matches) throw new GithubError(operation, null);
  return value as Kinds[K];
}

/** Like `field`, but `null` and absence read as null. */
export function nullable<K extends keyof Kinds>(data: unknown, key: string, kind: K, operation: string): Kinds[K] | null {
  const value = isRecord(data) ? data[key] : undefined;
  return value === null || value === undefined ? null : field(data, key, kind, operation);
}

function stringRecord(value: Record<string, unknown>, operation: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, level] of Object.entries(value)) {
    if (typeof level !== "string") throw new GithubError(operation, null);
    out[key] = level;
  }
  return out;
}
