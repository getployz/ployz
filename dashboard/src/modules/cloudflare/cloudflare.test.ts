import { Effect, Redacted, type Schema } from "effect";
import { describe, expect, it } from "vitest";
import { checkEditPermissions, CloudflareError, CloudflareHttp, createRecord, createTunnel, deleteTunnel, listZones, refusalWords, tunnelToken } from "#/modules/cloudflare/api.server";
import { isTunnelToken } from "#/modules/cloudflare/tunnel-token";
import { normalizeZone, wildcardRecord, zoneProblem } from "#/modules/cloudflare/zone";

/** A token shaped as Cloudflare mints them: base64 JSON naming the account, the tunnel and the connector's secret. */
const tunnelTokenOf = (claims: { readonly a?: string; readonly t?: string; readonly s?: string }) => Buffer.from(JSON.stringify(claims)).toString("base64");
/** Base64 JSON, but not a tunnel token: a connector can't start on it. */
const NOT_A_TOKEN = Buffer.from(JSON.stringify({ not: "a token" })).toString("base64");
const TUNNEL_TOKEN = tunnelTokenOf({ a: "0123456789abcdef0123456789abcdef", t: "bd0e0af1-d7e0-4d84-8bc3-d1ca9338a9ad", s: Buffer.alloc(64, 7).toString("base64") });

describe("the tunnel token's shape", () => {
  it("takes what Cloudflare mints and refuses anything a connector can't start on", () => {
    expect(isTunnelToken(TUNNEL_TOKEN)).toBe(true);
    expect(isTunnelToken("")).toBe(false);
    expect(isTunnelToken("not base64!")).toBe(false);
    expect(isTunnelToken(Buffer.from("[]").toString("base64"))).toBe(false);
    expect(isTunnelToken(tunnelTokenOf({ a: "", t: "bd0e0af1-d7e0-4d84-8bc3-d1ca9338a9ad", s: "c2VjcmV0" }))).toBe(false);
    expect(isTunnelToken(tunnelTokenOf({ a: "acct", t: "not-a-uuid", s: "c2VjcmV0" }))).toBe(false);
    expect(isTunnelToken(tunnelTokenOf({ a: "acct", t: "bd0e0af1-d7e0-4d84-8bc3-d1ca9338a9ad", s: "" }))).toBe(false);
    expect(isTunnelToken(tunnelTokenOf({ a: "acct", t: "bd0e0af1-d7e0-4d84-8bc3-d1ca9338a9ad", s: "%%%" }))).toBe(false);
  });
});

const ZONES = ["example.com", "apps.dev"];
const zone = { id: "z1", name: "example.com", accountId: "a1" };
const token = Redacted.make("secret-api-token");

describe("the zone rule", () => {
  it("takes a listed zone, however it was typed", () => {
    expect(normalizeZone(" HTTPS://*.Example.com. ")).toBe("example.com");
    expect(zoneProblem("Example.com", ZONES)).toBeNull();
  });

  it("refuses a subdomain of a listed zone and explains the one-level rule", () => {
    expect(zoneProblem("apps.example.com", ZONES)).toBe(
      "Use example.com, the zone apps.example.com belongs to.",
    );
  });

  it("refuses what isn't a zone of this account or isn't a domain at all", () => {
    expect(zoneProblem("other.org", ZONES)).toBe("This token can't see other.org. Pick one of its zones.");
    expect(zoneProblem("localhost", ZONES)).toBe("Use a domain name such as example.com.");
    expect(zoneProblem("", ZONES)).toBe("Pick one of your Cloudflare zones.");
  });
});

describe("the records setup writes", () => {
  it("is one proxied wildcard CNAME to the tunnel, never the apex", () => {
    expect(wildcardRecord("example.com", "t1")).toEqual({ type: "CNAME", name: "*.example.com", content: "t1.cfargotunnel.com", proxied: true, ttl: 1 });
  });
});

type Seen = { readonly method: string; readonly path: string; readonly body: Schema.Json | undefined; readonly authorization: string | null };
type Answer = { readonly status: number; readonly result?: Schema.Json };

/** A Cloudflare that answers from `answers`, keyed `METHOD path`, and records every call. */
function cloudflare(answers: Record<string, Answer>) {
  const seen: Seen[] = [];
  const fetch = (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(String(input));
    const path = `${url.pathname.replace("/client/v4", "")}${url.search}`;
    const method = init?.method ?? "GET";
    const body: Schema.Json | undefined = init?.body == null ? undefined : JSON.parse(String(init.body));
    seen.push({ method, path, body, authorization: new Headers(init?.headers).get("authorization") });
    const answer = answers[`${method} ${path}`] ?? { status: 404 };
    const ok = answer.status < 300;
    return Promise.resolve(new Response(JSON.stringify({ success: ok, errors: ok ? [] : [{ code: 1, message: "no" }], result: answer.result }), { status: answer.status }));
  };
  return { http: new CloudflareHttp({ fetch }), seen };
}

describe("the Cloudflare API calls", () => {
  it("lists the zones a token can see, sending the token only as the bearer", async () => {
    const { http, seen } = cloudflare({ "GET /zones?per_page=50&status=active": { status: 200, result: [{ id: "z1", name: "example.com", account: { id: "a1" } }] } });
    expect(await Effect.runPromise(listZones(http, token))).toEqual([zone]);
    expect(seen).toHaveLength(1);
    expect(seen[0]?.authorization).toBe("Bearer secret-api-token");
    expect(JSON.stringify(seen[0]?.body ?? null)).not.toContain("secret-api-token");
  });

  it("names the missing permission and writes nothing when the token can't edit DNS", async () => {
    const { http, seen } = cloudflare({ "POST /zones/z1/dns_records": { status: 403 } });
    const failure = await Effect.runPromise(checkEditPermissions(http, token, zone).pipe(Effect.flip));
    expect(failure).toBeInstanceOf(CloudflareError);
    expect(refusalWords(failure)).toBe("That token is missing DNS Edit. Add the permission and paste the token again.");
    expect(seen.every(({ body }) => JSON.stringify(body) === "{}")).toBe(true);
  });

  it("passes a token that is allowed to write without creating anything", async () => {
    const { http, seen } = cloudflare({ "POST /zones/z1/dns_records": { status: 400 }, "POST /accounts/a1/cfd_tunnel": { status: 400 } });
    await Effect.runPromise(checkEditPermissions(http, token, zone));
    expect(seen.map(({ method, path }) => `${method} ${path}`)).toEqual(["POST /zones/z1/dns_records", "POST /accounts/a1/cfd_tunnel"]);
  });

  it("says a rejected token needs copying again, without echoing it", async () => {
    const { http } = cloudflare({ "GET /zones?per_page=50&status=active": { status: 401 } });
    const failure = await Effect.runPromise(listZones(http, token).pipe(Effect.flip));
    expect(refusalWords(failure)).toBe("Cloudflare didn't accept that token. Copy it again from your Cloudflare profile.");
    expect(JSON.stringify(failure)).not.toContain("secret-api-token");
  });

  it("hands back the tunnel without a token when Cloudflare sends none, and reads it on request", async () => {
    const { http } = cloudflare({
      "POST /accounts/a1/cfd_tunnel": { status: 200, result: { id: "t1" } },
      "GET /accounts/a1/cfd_tunnel/t1/token": { status: 200, result: TUNNEL_TOKEN },
    });
    const made = await Effect.runPromise(createTunnel(http, token, "a1", "ployz-acme"));
    expect(made).toEqual({ id: "t1", token: null });
    expect(Redacted.value(await Effect.runPromise(tunnelToken(http, token, "a1", "t1")))).toBe(TUNNEL_TOKEN);
  });

  it("treats a token a connector can't start on as none, since the tunnel exists either way, and refuses it on request", async () => {
    const { http } = cloudflare({
      "POST /accounts/a1/cfd_tunnel": { status: 200, result: { id: "t1", token: NOT_A_TOKEN } },
      "GET /accounts/a1/cfd_tunnel/t1/token": { status: 200, result: "plain" },
    });
    expect(await Effect.runPromise(createTunnel(http, token, "a1", "ployz-acme"))).toEqual({ id: "t1", token: null });
    const read = await Effect.runPromise(tunnelToken(http, token, "a1", "t1").pipe(Effect.flip));
    expect(read.kind).toBe("unusable_answer");
    expect(refusalWords(read)).toBe("Cloudflare sent an answer Ployz couldn't read. Try again in a moment.");
  });

  it("says whether a deleted tunnel is gone, and that one already gone is", async () => {
    const { http } = cloudflare({ "DELETE /accounts/a1/cfd_tunnel/t1": { status: 200, result: {} } });
    expect(await Effect.runPromise(deleteTunnel(http, token, "a1", "t1"))).toBe(true);
    expect(await Effect.runPromise(deleteTunnel(http, token, "a1", "t2"))).toBe(false);
  });

  it("creates the one record it is given", async () => {
    const { http, seen } = cloudflare({ "POST /zones/z1/dns_records": { status: 200, result: { id: "r1" } } });
    await Effect.runPromise(createRecord(http, token, "z1", wildcardRecord("example.com", "t1")));
    expect(seen.map(({ body }) => body)).toEqual([wildcardRecord("example.com", "t1")]);
  });
});
