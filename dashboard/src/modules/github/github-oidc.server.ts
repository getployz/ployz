import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { Cache, Context, Data, Effect, Layer, Schema } from "effect";

/** GitHub Actions' OIDC issuer: every runner token names it, and its JWKS signs them. */
export const GITHUB_OIDC_ISSUER = "https://token.actions.githubusercontent.com";

export class GithubOidcRejected extends Data.TaggedError("GithubOidcRejected")<{ readonly message: string }> {}

const jwkSchema = Schema.Record(Schema.String, Schema.Unknown);
type Jwk = typeof jwkSchema.Type;
const jwksSchema = Schema.Struct({ keys: Schema.Array(jwkSchema) });

/** The issuer's signing keys, as JWKs. Tests provide their own. */
export class GithubOidcKeys extends Context.Service<GithubOidcKeys, {
  readonly keys: Effect.Effect<readonly Jwk[], GithubOidcRejected>;
}>()("ployz/GithubOidcKeys") {}

export const GithubOidcKeysLive = Layer.effect(GithubOidcKeys, Effect.gen(function* () {
  // ponytail: a key GitHub rotated in within the TTL is refused until the cache expires; refetch on an unknown kid if that bites.
  const cache = yield* Cache.make({
    capacity: 1,
    timeToLive: "10 minutes",
    lookup: () => Effect.tryPromise({
      try: async (signal) => {
        const response = await fetch(`${GITHUB_OIDC_ISSUER}/.well-known/jwks`, { signal });
        if (!response.ok) throw new Error(`JWKS request failed: ${response.status}`);
        return Schema.decodeUnknownSync(jwksSchema)(await response.json()).keys;
      },
      catch: () => new GithubOidcRejected({ message: "GitHub's OIDC keys are unavailable." }),
    }),
  });
  return { keys: Cache.get(cache, "jwks").pipe(Effect.tapError(() => Cache.invalidate(cache, "jwks"))) };
}));

/** The claims Cloud checks. GitHub sends ids as strings. */
const claimsSchema = Schema.Struct({
  iss: Schema.Literal(GITHUB_OIDC_ISSUER),
  aud: Schema.Union([Schema.String, Schema.Array(Schema.String)]),
  exp: Schema.Number,
  nbf: Schema.optional(Schema.Number),
  repository_id: Schema.String,
  job_workflow_ref: Schema.String,
  run_id: Schema.String,
  event_name: Schema.String,
});
export type GithubOidcClaims = typeof claimsSchema.Type;

const headerSchema = Schema.Struct({ alg: Schema.Literal("RS256"), kid: Schema.String });

/** Verify a runner's OIDC token: RS256 by one of GitHub's keys, issued for `audience`, not expired. */
export const verifyGithubOidcToken = Effect.fn("Github.verifyOidcToken")(function* (token: string, audience: string) {
  const reject = (message: string) => new GithubOidcRejected({ message });
  const [header, payload, signature, ...rest] = token.split(".");
  if (!header || !payload || !signature || rest.length) return yield* reject("The token is not a JWT.");
  const decodePart = <S extends Schema.Top>(schema: S, part: string, message: string) =>
    Schema.decodeUnknownEffect(Schema.fromJsonString(schema))(Buffer.from(part, "base64url").toString("utf8"))
      .pipe(Effect.mapError(() => reject(message)));
  const { kid } = yield* decodePart(headerSchema, header, "The token is not RS256.");
  const jwk = (yield* (yield* GithubOidcKeys).keys).find((key) => key["kid"] === kid);
  if (!jwk) return yield* reject("The token's signing key is unknown.");
  const valid = yield* Effect.try({
    try: () => crypto.verify("RSA-SHA256", Buffer.from(`${header}.${payload}`), crypto.createPublicKey({ key: jwk, format: "jwk" }), Buffer.from(signature, "base64url")),
    catch: () => reject("The token's signing key is unusable."),
  });
  if (!valid) return yield* reject("The token's signature is invalid.");
  const claims = yield* decodePart(claimsSchema, payload, "The token is not from GitHub Actions.");
  const now = Date.now() / 1000;
  if (claims.exp < now || (claims.nbf ?? 0) > now + 60) return yield* reject("The token has expired.");
  if (![claims.aud].flat().includes(audience)) return yield* reject("The token is for another audience.");
  return claims;
});
