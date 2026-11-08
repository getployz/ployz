import { Effect, Redacted, type Schema } from "effect";
import { describe, expect, it } from "vitest";
import { checkEditPermissions, CloudflareError, CloudflareHttp, createRecord, listZones, refusalWords } from "#/modules/cloudflare/api.server";
import { normalizeZone, wildcardRecord, zoneProblem } from "#/modules/cloudflare/zone";

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
      "apps.example.com is under your zone example.com. Use example.com. Addresses go one level below the zone.",
    );
  });

  it("refuses what isn't a zone of this account or isn't a domain at all", () => {
    expect(zoneProblem("other.org", ZONES)).toBe("other.org isn't a zone in this Cloudflare account.");
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
    expect(refusalWords(failure)).toBe('That token can\'t do this. Add the "Zone: DNS: Edit" permission and paste it again. Nothing was changed.');
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

  it("creates the one record it is given", async () => {
    const { http, seen } = cloudflare({ "POST /zones/z1/dns_records": { status: 200, result: {} } });
    await Effect.runPromise(createRecord(http, token, "z1", wildcardRecord("example.com", "t1")));
    expect(seen.map(({ body }) => body)).toEqual([wildcardRecord("example.com", "t1")]);
  });
});
