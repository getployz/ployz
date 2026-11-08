import "@tanstack/react-start/server-only";
import { Data, Effect, Redacted, Schema } from "effect";
import { isTunnelToken } from "#/modules/cloudflare/tunnel-token";
import { type CloudflareZone, PERMISSIONS, type PermissionKey } from "#/modules/cloudflare/zone";

/**
 * Every call Cloud makes to Cloudflare lives in this file, made with a token the user pasted for one setup. The token
 * stays `Redacted`, goes only into the Authorization header, and appears in no error, log or stored row.
 */

const BASE = "https://api.cloudflare.com/client/v4";
const REQUEST_TIMEOUT_MS = 15_000;

export class CloudflareError extends Data.TaggedError("CloudflareError")<{
  /** `unusable_answer`: Cloudflare answered, but not with what the operation needs, such as a tunnel token a connector can't start on. */
  readonly kind: "token_rejected" | "missing_permission" | "refused" | "unreachable" | "unusable_answer";
  readonly operation: string;
  readonly permission?: PermissionKey;
  readonly detail?: string;
}> {}

export type CloudflareFetch = typeof fetch;

/** The fetch Cloud calls Cloudflare with; tests swap it for a mock of the API. */
export class CloudflareHttp extends Data.TaggedClass("CloudflareHttp")<{ readonly fetch: CloudflareFetch }> {}

const Envelope = Schema.Struct({
  success: Schema.Boolean,
  errors: Schema.optional(Schema.Array(Schema.Struct({ code: Schema.Number, message: Schema.String }))),
  result: Schema.optional(Schema.Json),
});

type Payload = Schema.Json | undefined;
type Answer = { readonly status: number; readonly success: boolean; readonly result: Payload; readonly detail: string };

const call = (
  http: CloudflareHttp,
  token: Redacted.Redacted<string>,
  operation: string,
  method: "GET" | "POST" | "DELETE",
  path: string,
  body?: Schema.Json,
) =>
  Effect.tryPromise({
    try: async (signal): Promise<Answer> => {
      const headers = new Headers({ authorization: `Bearer ${Redacted.value(token)}` });
      if (body !== undefined) headers.set("content-type", "application/json");
      const response = await http.fetch(`${BASE}${path}`, {
        method,
        signal: AbortSignal.any([signal, AbortSignal.timeout(REQUEST_TIMEOUT_MS)]),
        headers,
        body: body === undefined ? null : JSON.stringify(body),
      });
      const text = await response.text();
      const parsed = Schema.decodeUnknownOption(Envelope)(text === "" ? {} : JSON.parse(text));
      const envelope = parsed._tag === "Some" ? parsed.value : { success: false, errors: [], result: undefined };
      return {
        status: response.status,
        success: response.ok && envelope.success,
        result: envelope.result,
        detail: (envelope.errors ?? []).map((error) => error.message).join("; "),
      };
    },
    catch: () => new CloudflareError({ kind: "unreachable", operation }),
  });

/** A refusal read from the status: a rejected token or one missing `permission`. */
const refusal = (answer: Answer, operation: string, permission: PermissionKey) => {
  if (answer.status === 401) return new CloudflareError({ kind: "token_rejected", operation });
  if (answer.status === 403) return new CloudflareError({ kind: "missing_permission", operation, permission });
  return new CloudflareError({ kind: "refused", operation, detail: answer.detail });
};

const Zones = Schema.Array(Schema.Struct({
  id: Schema.String,
  name: Schema.String,
  account: Schema.Struct({ id: Schema.String }),
}));

/** The account's active zones. A rejected token or one that can't read zones fails here, before anything changes. */
export const listZones = Effect.fn("Cloudflare.listZones")(function* (http: CloudflareHttp, token: Redacted.Redacted<string>) {
  const answer = yield* call(http, token, "list zones", "GET", "/zones?per_page=50&status=active");
  if (!answer.success) return yield* refusal(answer, "list zones", "zones");
  const zones = yield* Schema.decodeUnknownEffect(Zones)(answer.result).pipe(
    Effect.mapError(() => new CloudflareError({ kind: "unusable_answer", operation: "list zones" })),
  );
  return zones.map((zone): CloudflareZone => ({ id: zone.id, name: zone.name, accountId: zone.account.id }));
});

/**
 * Whether the token may write at all, without writing: an empty body fails Cloudflare's validation (400) once the
 * token is allowed through, and its authorization (403) before.
 */
const mayWrite = Effect.fn("Cloudflare.mayWrite")(function* (
  http: CloudflareHttp, token: Redacted.Redacted<string>, path: string, permission: PermissionKey,
) {
  const answer = yield* call(http, token, "check permission", "POST", path, {});
  if (answer.status === 401 || answer.status === 403) return yield* refusal(answer, "check permission", permission);
});

export const checkEditPermissions = Effect.fn("Cloudflare.checkEditPermissions")(function* (
  http: CloudflareHttp, token: Redacted.Redacted<string>, zone: CloudflareZone,
) {
  yield* mayWrite(http, token, `/zones/${zone.id}/dns_records`, "dns");
  yield* mayWrite(http, token, `/accounts/${zone.accountId}/cfd_tunnel`, "tunnel");
});

const Tunnels = Schema.Array(Schema.Struct({ id: Schema.String }));
const Tunnel = Schema.Struct({ id: Schema.String, token: Schema.optional(Schema.String) });
const Records = Schema.Array(Schema.Struct({ id: Schema.String, content: Schema.String }));

const decoded = <S extends Schema.Top>(schema: S, operation: string, value: Payload) =>
  Schema.decodeUnknownEffect(schema)(value).pipe(Effect.mapError(() => new CloudflareError({ kind: "unusable_answer", operation })));

/** The tunnel token Cloudflare sent, once it is one a connector can start on. */
const usableToken = (operation: string, value: string) =>
  isTunnelToken(value) ? Effect.succeed(Redacted.make(value)) : Effect.fail(new CloudflareError({ kind: "unusable_answer", operation }));

/** The tunnel named `name`, when the account holds one. */
export const findTunnel = Effect.fn("Cloudflare.findTunnel")(function* (
  http: CloudflareHttp, token: Redacted.Redacted<string>, accountId: string, name: string,
) {
  const answer = yield* call(http, token, "find tunnel", "GET", `/accounts/${accountId}/cfd_tunnel?is_deleted=false&name=${encodeURIComponent(name)}`);
  if (!answer.success) return yield* refusal(answer, "find tunnel", "tunnel");
  const [found] = yield* decoded(Tunnels, "find tunnel", answer.result);
  return found?.id ?? null;
});

/**
 * A tunnel whose routes the connector's local config owns, so no Cloudflare ingress rule is stored for it. Cloudflare
 * sends the connector's token with the tunnel today; when it doesn't, or sends one a connector can't start on, `token`
 * is null and {@link tunnelToken} reads it.
 * The tunnel exists once this returns, whatever the token, so a caller that stops after it deletes it.
 */
export const createTunnel = Effect.fn("Cloudflare.createTunnel")(function* (
  http: CloudflareHttp, token: Redacted.Redacted<string>, accountId: string, name: string,
) {
  const answer = yield* call(http, token, "create tunnel", "POST", `/accounts/${accountId}/cfd_tunnel`, { name, config_src: "local" });
  if (!answer.success) return yield* refusal(answer, "create tunnel", "tunnel");
  const tunnel = yield* decoded(Tunnel, "create tunnel", answer.result);
  // The tunnel exists from here on, so nothing may fail past this point: the caller needs the id to undo it.
  return { id: tunnel.id, token: tunnel.token !== undefined && isTunnelToken(tunnel.token) ? Redacted.make(tunnel.token) : null };
});

/** The token a connector runs the tunnel with. */
export const tunnelToken = Effect.fn("Cloudflare.tunnelToken")(function* (
  http: CloudflareHttp, token: Redacted.Redacted<string>, accountId: string, tunnelId: string,
) {
  const answer = yield* call(http, token, "read tunnel token", "GET", `/accounts/${accountId}/cfd_tunnel/${tunnelId}/token`);
  if (!answer.success) return yield* refusal(answer, "read tunnel token", "tunnel");
  return yield* usableToken("read tunnel token", yield* decoded(Schema.String, "read tunnel token", answer.result));
});

/**
 * Whether the tunnel is gone afterwards. Cloudflare refuses to delete a tunnel whose connectors still hold connections
 * (its error 1022), and a connector just stopped keeps its connections registered for minutes, so they are cleaned up
 * first. Deleting one already deleted succeeds, so this converges.
 */
export const deleteTunnel = (http: CloudflareHttp, token: Redacted.Redacted<string>, accountId: string, tunnelId: string) =>
  call(http, token, "clean up tunnel connections", "DELETE", `/accounts/${accountId}/cfd_tunnel/${tunnelId}/connections`).pipe(
    Effect.ignore,
    Effect.andThen(call(http, token, "delete tunnel", "DELETE", `/accounts/${accountId}/cfd_tunnel/${tunnelId}`)),
    Effect.map((answer) => answer.success),
    Effect.catch(() => Effect.succeed(false)),
  );

/** The CNAME records already named `name` in the zone. */
export const findRecords = Effect.fn("Cloudflare.findRecords")(function* (
  http: CloudflareHttp, token: Redacted.Redacted<string>, zoneId: string, name: string,
) {
  const answer = yield* call(http, token, "find records", "GET", `/zones/${zoneId}/dns_records?name=${encodeURIComponent(name)}`);
  if (!answer.success) return yield* refusal(answer, "find records", "dns");
  return yield* decoded(Records, "find records", answer.result);
});

export const createRecord = Effect.fn("Cloudflare.createRecord")(function* (
  http: CloudflareHttp, token: Redacted.Redacted<string>, zoneId: string, record: Readonly<Record<string, Schema.Json>>,
) {
  const answer = yield* call(http, token, "create record", "POST", `/zones/${zoneId}/dns_records`, record);
  if (!answer.success) return yield* refusal(answer, "create record", "dns");
});

/** Cloudflare's refusal in words, for the message a user reads. What it left behind is the caller's to say. */
export const refusalWords = (error: CloudflareError) => {
  switch (error.kind) {
    case "token_rejected": return "Cloudflare didn't accept that token. Copy it again from your Cloudflare profile.";
    case "missing_permission": return `That token can't do this. Add the "${PERMISSIONS[error.permission ?? "zones"]}" permission and paste it again.`;
    case "unreachable": return "Cloudflare didn't answer. Try again in a moment.";
    case "refused": return `Cloudflare refused the ${error.operation}${error.detail ? `: ${error.detail}` : ""}.`;
    case "unusable_answer": return `Cloudflare's answer to the ${error.operation} wasn't one Ployz can use. Try again in a moment.`;
  }
};
