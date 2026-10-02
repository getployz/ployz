import "@tanstack/react-start/server-only";
import {
  Config,
  Context,
  Data,
  Effect,
  Layer,
  Option,
  type Redacted,
  Result,
  Schema,
  SchemaIssue,
} from "effect";

const Uuid = Schema.String.check(Schema.isUUID());
const DEFAULT_HOSTED_DNS_URL = new URL("https://dns.ployz.app/");
const DEFAULT_POSTHOG_HOST = new URL("https://us.i.posthog.com");
const NonEmptySecret = Schema.Redacted(Schema.NonEmptyString);
const EncryptionSecret = Schema.Redacted(
  Schema.String.check(Schema.isMinLength(32)),
);

export type PolarConfiguration =
  | { readonly mode: "self_hosted" }
  | {
      readonly mode: "hosted";
      readonly accessToken: Redacted.Redacted<string>;
      readonly webhookSecret: Redacted.Redacted<string>;
      readonly server: "production" | "sandbox";
      readonly productId: string;
    };

export class InvalidConfiguration extends Data.TaggedError(
  "InvalidConfiguration",
)<{ readonly message: string }> {}

const optional = <A>(config: Config.Config<A>) =>
  Config.option(config).pipe(Config.map(Option.getOrUndefined));

const rawFields = {
  nodeEnv: Config.literals(["development", "test", "production"], "NODE_ENV").pipe(
    Config.withDefault("production"),
  ),
  databaseUrl: Config.url("DATABASE_URL"),
  appUrl: Config.url("APP_URL"),
  port: Config.port("PORT").pipe(Config.withDefault(3000)),
  betterAuthSecret: Config.schema(NonEmptySecret, "BETTER_AUTH_SECRET"),
  betterAuthTrustedOrigins: optional(
    Config.nonEmptyString("BETTER_AUTH_TRUSTED_ORIGINS"),
  ),
  // Railway injects this; its edge sets X-Real-IP to the client's address, overwriting any sent.
  railwayEnvironmentId: optional(Config.nonEmptyString("RAILWAY_ENVIRONMENT_ID")),
  githubClientId: Config.nonEmptyString("GITHUB_CLIENT_ID"),
  githubClientSecret: Config.schema(NonEmptySecret, "GITHUB_CLIENT_SECRET"),
  githubAppId: Config.nonEmptyString("GITHUB_APP_ID"),
  githubAppPrivateKey: Config.schema(NonEmptySecret, "GITHUB_APP_PRIVATE_KEY"),
  githubAppSlug: Config.nonEmptyString("GITHUB_APP_SLUG"),
  githubAppWebhookSecret: Config.schema(
    NonEmptySecret,
    "GITHUB_APP_WEBHOOK_SECRET",
  ),
  polarAccessToken: optional(
    Config.schema(NonEmptySecret, "POLAR_ACCESS_TOKEN"),
  ),
  polarServer: Config.literals(["production", "sandbox"], "POLAR_SERVER").pipe(
    Config.withDefault("production"),
  ),
  polarWebhookSecret: optional(
    Config.schema(NonEmptySecret, "POLAR_WEBHOOK_SECRET"),
  ),
  polarProductId: optional(Config.schema(Uuid, "POLAR_PRODUCT_ID")),
  posthogKey: optional(Config.nonEmptyString("POSTHOG_KEY")),
  posthogHost: Config.url("POSTHOG_HOST").pipe(
    Config.withDefault(DEFAULT_POSTHOG_HOST),
  ),
  hostedDnsUrl: Config.url("PLOYZ_HOSTED_DNS_URL").pipe(
    Config.withDefault(DEFAULT_HOSTED_DNS_URL),
  ),
  hostedDnsMintKey: optional(
    Config.schema(NonEmptySecret, "PLOYZ_HOSTED_DNS_MINT_KEY"),
  ),
  marketingOrigin: optional(Config.url("MARKETING_ORIGIN")),
  inngestEventKey: Config.schema(NonEmptySecret, "INNGEST_EVENT_KEY"),
  inngestSigningKey: Config.schema(NonEmptySecret, "INNGEST_SIGNING_KEY"),
  encryptionSecret: Config.schema(EncryptionSecret, "APP_ENCRYPTION_SECRET"),
};
/**
 * Config.all stops at the first bad variable. Read every field once so one
 * ConfigError names all missing or malformed variables.
 */
const loadRawConfig = Effect.all(rawFields, { mode: "result" }).pipe(
  Effect.flatMap((results) => {
    const issues: Array<SchemaIssue.Issue> = [];
    for (const result of Object.values(results)) {
      if (result._tag === "Success") continue;
      const { cause } = result.failure;
      issues.push(
        Schema.isSchemaError(cause)
          ? cause.issue
          : new SchemaIssue.InvalidValue({ message: cause.message }),
      );
    }
    const [head, ...tail] = issues;
    return head === undefined
      ? Effect.fromResult(Result.all(results))
      : Effect.fail(
          new Config.ConfigError(
            new Schema.SchemaError(
              new SchemaIssue.Composite(Schema.Unknown.ast, [head, ...tail]),
            ),
          ),
        );
  }),
);

/** No Polar variables means a Self-hosted Cloud; a partial set is a mistake. */
const resolvePolarConfiguration = Effect.fn("Config.resolvePolar")(function* (
  input: Effect.Success<typeof loadRawConfig>,
) {
  const { polarAccessToken, polarWebhookSecret, polarProductId } = input;
  if (
    polarAccessToken === undefined &&
    polarWebhookSecret === undefined &&
    polarProductId === undefined
  ) {
    return { mode: "self_hosted" } as const;
  }
  if (
    polarAccessToken === undefined ||
    polarWebhookSecret === undefined ||
    polarProductId === undefined
  ) {
    return yield* new InvalidConfiguration({
      message:
        "Polar configuration must be entirely absent or include POLAR_ACCESS_TOKEN, POLAR_WEBHOOK_SECRET, and POLAR_PRODUCT_ID",
    });
  }
  return {
    mode: "hosted",
    accessToken: polarAccessToken,
    webhookSecret: polarWebhookSecret,
    server: input.polarServer,
    productId: polarProductId,
  } as const;
});

const makeAppConfig = Effect.gen(function* () {
  const raw = yield* loadRawConfig;
  const polar = yield* resolvePolarConfiguration(raw);

  return {
    nodeEnv: raw.nodeEnv,
    app: { url: raw.appUrl, port: raw.port },
    database: { url: raw.databaseUrl },
    auth: {
      secret: raw.betterAuthSecret,
      trustedOrigins: raw.betterAuthTrustedOrigins,
      url: raw.appUrl,
      // Elsewhere better-auth's default: a single-address X-Forwarded-For.
      clientIpHeaders: raw.railwayEnvironmentId === undefined ? undefined : ["x-real-ip"],
    },
    github: {
      clientId: raw.githubClientId,
      clientSecret: raw.githubClientSecret,
      appId: raw.githubAppId,
      appPrivateKey: raw.githubAppPrivateKey,
      appSlug: raw.githubAppSlug,
      appWebhookSecret: raw.githubAppWebhookSecret,
    },
    polar,
    // Unset (Self-hosted Cloud): no product analytics. The project key is public; browsers send it too.
    posthog: raw.posthogKey === undefined
      ? null
      : { key: raw.posthogKey, host: raw.posthogHost.origin },
    ployz: {
      hostedDnsUrl: raw.hostedDnsUrl,
      hostedDnsMintKey: raw.hostedDnsMintKey,
    },
    inngest: {
      eventKey: raw.inngestEventKey,
      signingKey: raw.inngestSigningKey,
    },
    // Unset (Self-hosted Cloud): no marketing proxy; / sends signed-out visitors to /auth.
    marketingOrigin: raw.marketingOrigin,
    encryptionSecret: raw.encryptionSecret,
  } as const;
});

export class AppConfig extends Context.Service<AppConfig>()("ployz/AppConfig", {
  make: makeAppConfig,
}) {
  static readonly layer = Layer.effect(this, this.make);
}
