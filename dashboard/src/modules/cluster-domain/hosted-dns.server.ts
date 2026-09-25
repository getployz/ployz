import "@tanstack/react-start/server-only";
import { Data, Effect, Schema } from "effect";

/**
 * The Hosted DNS HTTP client: every call Cloud makes to Hosted DNS lives in this file.
 * The contract is the getployz/hosted-dns API (spec #1015).
 * `POST /domains` mints a name and a token shown once, authorized by the optional mint key;
 * every other call uses the per-name bearer token.
 */

export class HostedDnsError extends Data.TaggedError("HostedDnsError")<{
  readonly operation: string;
  readonly status?: number;
  readonly cause?: unknown;
}> {}

/** One reserved name and where and how to call Hosted DNS for it. */
export type HostedDomainTarget = {
  readonly endpoint: string;
  readonly name: string;
  readonly token: string;
};

const REQUEST_TIMEOUT_MS = 10_000;
/** Issuance waits on Route 53 sync and CA validation: up to about ten minutes on the Hosted DNS side. */
const CERTIFICATE_TIMEOUT_MS = 15 * 60_000;

const request = (
  operation: string,
  method: "POST" | "PUT" | "DELETE",
  url: string,
  init: { token: string | undefined; body?: unknown; timeoutMs?: number },
) =>
  Effect.tryPromise({
    try: async (signal) => {
      const headers = new Headers(init.body === undefined ? {} : { "content-type": "application/json" });
      if (init.token !== undefined) headers.set("authorization", `Bearer ${init.token}`);
      const response = await fetch(url, {
        method,
        signal: AbortSignal.any([signal, AbortSignal.timeout(init.timeoutMs ?? REQUEST_TIMEOUT_MS)]),
        headers,
        body: init.body === undefined ? null : JSON.stringify(init.body),
      });
      if (!response.ok) throw new HostedDnsError({ operation, status: response.status });
      const text = await response.text();
      const body: unknown = text === "" ? null : JSON.parse(text);
      return body;
    },
    catch: (cause) => cause instanceof HostedDnsError ? cause : new HostedDnsError({ operation, cause }),
  });

const domainsUrl = (endpoint: string, ...path: string[]) =>
  [endpoint.replace(/\/+$/u, ""), "domains", ...path.map(encodeURIComponent)].join("/");

const Reservation = Schema.Struct({ name: Schema.NonEmptyString, token: Schema.NonEmptyString });

/** Mints a name; Hosted DNS suffixes or replaces `preferred` as it sees fit and returns what it granted. */
export const reserveHostedDomain = Effect.fn("HostedDns.reserve")(function* (input: {
  readonly endpoint: string;
  readonly preferred: string;
  readonly mintKey: string | undefined;
}) {
  const body = yield* request("reserve", "POST", domainsUrl(input.endpoint), { token: input.mintKey, body: { preferred: input.preferred } });
  return yield* Schema.decodeUnknownEffect(Reservation)(body).pipe(
    Effect.mapError((cause) => new HostedDnsError({ operation: "reserve", cause })),
  );
});

/** Retires the name forever. */
export const releaseHostedDomain = Effect.fn("HostedDns.release")(function* (input: HostedDomainTarget) {
  yield* request("release", "DELETE", domainsUrl(input.endpoint, input.name), { token: input.token });
});

/** Replaces the apex A/AAAA set. Hosted DNS refuses an empty set, so callers skip the call instead. */
export const putHostedDomainRecords = Effect.fn("HostedDns.putRecords")(function* (input: HostedDomainTarget & {
  readonly a: readonly string[];
  readonly aaaa: readonly string[];
}) {
  yield* request("put records", "PUT", domainsUrl(input.endpoint, input.name, "records"), {
    token: input.token,
    body: { a: input.a, aaaa: input.aaaa },
  });
});

/** Extends the lease by seven days; a name whose lease runs out is retired. Any other bearer call renews it too. */
export const renewHostedDomainLease = Effect.fn("HostedDns.renewLease")(function* (input: HostedDomainTarget) {
  yield* request("renew lease", "POST", domainsUrl(input.endpoint, input.name, "lease"), { token: input.token });
});

const IssuedCertificate = Schema.Struct({ certificate_chain_pem: Schema.NonEmptyString });

/** Obtains the PEM chain for a CSR naming exactly `name` and `*.name`. Synchronous on the Hosted DNS side; can take minutes. */
export const requestHostedDomainCertificate = Effect.fn("HostedDns.requestCertificate")(function* (input: HostedDomainTarget & { readonly csr: string }) {
  const body = yield* request("request certificate", "POST", domainsUrl(input.endpoint, input.name, "certificate"), {
    token: input.token,
    body: { csr: input.csr },
    timeoutMs: CERTIFICATE_TIMEOUT_MS,
  });
  const issued = yield* Schema.decodeUnknownEffect(IssuedCertificate)(body).pipe(
    Effect.mapError((cause) => new HostedDnsError({ operation: "request certificate", cause })),
  );
  return issued.certificate_chain_pem;
});
