import "@tanstack/react-start/server-only";
import {
  Config,
  Context,
  Data,
  Effect,
  Layer,
  Option,
  type Redacted,
  Schema,
} from "effect";

const Uuid = Schema.String.check(Schema.isUUID());
const DEFAULT_HOSTED_DNS_URL = new URL("https://dns.ployz.app/");
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

const rawConfig = Config.all({
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
  hostedDnsUrl: Config.url("PLOYZ_HOSTED_DNS_URL").pipe(
    Config.withDefault(DEFAULT_HOSTED_DNS_URL),
  ),
  hostedDnsMintKey: optional(
    Config.schema(NonEmptySecret, "PLOYZ_HOSTED_DNS_MINT_KEY"),
  ),
  inngestEventKey: Config.schema(NonEmptySecret, "INNGEST_EVENT_KEY"),
  inngestSigningKey: Config.schema(NonEmptySecret, "INNGEST_SIGNING_KEY"),
  encryptionSecret: Config.schema(EncryptionSecret, "APP_ENCRYPTION_SECRET"),
});

/** No Polar variables means a Self-hosted Cloud; a partial set is a mistake. */
const resolvePolarConfiguration = Effect.fn("Config.resolvePolar")(function* (
  input: Config.Success<typeof rawConfig>,
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
  const raw = yield* rawConfig;
  const polar = yield* resolvePolarConfiguration(raw);
  const appUrl = raw.appUrl.href.replace(/\/$/, "");

  return {
    nodeEnv: raw.nodeEnv,
    app: { url: raw.appUrl, port: raw.port },
    database: { url: raw.databaseUrl },
    auth: {
      secret: raw.betterAuthSecret,
      trustedOrigins: raw.betterAuthTrustedOrigins,
      url: raw.appUrl,
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
    polarSuccessUrl: `${appUrl}/cloud?checkout_id={CHECKOUT_ID}`,
    ployz: {
      hostedDnsUrl: raw.hostedDnsUrl,
      hostedDnsMintKey: raw.hostedDnsMintKey,
    },
    inngest: {
      eventKey: raw.inngestEventKey,
      signingKey: raw.inngestSigningKey,
    },
    encryptionSecret: raw.encryptionSecret,
  } as const;
});

export class AppConfig extends Context.Service<AppConfig>()("ployz/AppConfig", {
  make: makeAppConfig,
}) {
  static readonly layer = Layer.effect(this, this.make);
}
