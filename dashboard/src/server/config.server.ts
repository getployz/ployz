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

const DEFAULT_INSTALLER_URL = new URL("https://ployz.sh/");
const Sha256 = Schema.String.check(
  Schema.isPattern(/^[a-fA-F0-9]{64}$/),
);
const Uuid = Schema.String.check(Schema.isUUID());
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
      readonly productIds: {
        readonly free: string;
        readonly solo: string;
        readonly teams: string;
      };
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
  githubAppId: optional(Config.nonEmptyString("GITHUB_APP_ID")),
  githubAppPrivateKey: optional(
    Config.schema(NonEmptySecret, "GITHUB_APP_PRIVATE_KEY"),
  ),
  githubAppSlug: optional(Config.nonEmptyString("GITHUB_APP_SLUG")),
  githubAppWebhookSecret: optional(
    Config.schema(NonEmptySecret, "GITHUB_APP_WEBHOOK_SECRET"),
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
  polarProductFreeId: optional(Config.schema(Uuid, "POLAR_PRODUCT_FREE_ID")),
  polarProductSoloId: optional(Config.schema(Uuid, "POLAR_PRODUCT_SOLO_ID")),
  polarProductTeamsId: optional(Config.schema(Uuid, "POLAR_PRODUCT_TEAMS_ID")),
  polarProductHobbyId: optional(Config.schema(Uuid, "POLAR_PRODUCT_HOBBY_ID")),
  polarProductProId: optional(Config.schema(Uuid, "POLAR_PRODUCT_PRO_ID")),
  installerUrl: Config.url("PLOYZ_INSTALLER_URL").pipe(
    Config.withDefault(DEFAULT_INSTALLER_URL),
  ),
  installerSha256: optional(Config.schema(Sha256, "PLOYZ_INSTALLER_SHA256")),
  inngestEventKey: optional(Config.schema(NonEmptySecret, "INNGEST_EVENT_KEY")),
  inngestSigningKey: optional(
    Config.schema(NonEmptySecret, "INNGEST_SIGNING_KEY"),
  ),
  inngestSigningKeyFallback: optional(
    Config.schema(NonEmptySecret, "INNGEST_SIGNING_KEY_FALLBACK"),
  ),
  encryptionSecret: Config.schema(EncryptionSecret, "APP_ENCRYPTION_SECRET"),
});

function invalid(message: string) {
  return Effect.fail(new InvalidConfiguration({ message }));
}

function resolveAlias(input: {
  readonly canonicalName: string;
  readonly canonicalId: string | undefined;
  readonly legacyName: string;
  readonly legacyId: string | undefined;
}) {
  if (
    input.canonicalId !== undefined &&
    input.legacyId !== undefined &&
    input.canonicalId !== input.legacyId
  ) {
    return invalid(
      `${input.canonicalName} conflicts with transitional alias ${input.legacyName}`,
    );
  }
  return Effect.succeed(input.canonicalId ?? input.legacyId);
}

const resolvePolarConfiguration = Effect.fn("Config.resolvePolar")(function* (
  input: Config.Success<typeof rawConfig>,
) {
  const solo = yield* resolveAlias({
    canonicalName: "POLAR_PRODUCT_SOLO_ID",
    canonicalId: input.polarProductSoloId,
    legacyName: "POLAR_PRODUCT_HOBBY_ID",
    legacyId: input.polarProductHobbyId,
  });
  const teams = yield* resolveAlias({
    canonicalName: "POLAR_PRODUCT_TEAMS_ID",
    canonicalId: input.polarProductTeamsId,
    legacyName: "POLAR_PRODUCT_PRO_ID",
    legacyId: input.polarProductProId,
  });
  const hostedValues = [
    input.polarAccessToken,
    input.polarWebhookSecret,
    input.polarProductFreeId,
    solo,
    teams,
  ];

  if (hostedValues.every((value) => value === undefined)) {
    return { mode: "self_hosted" } as const;
  }
  if (
    input.polarAccessToken === undefined ||
    input.polarWebhookSecret === undefined ||
    input.polarProductFreeId === undefined ||
    solo === undefined ||
    teams === undefined
  ) {
    return yield* invalid(
      "Polar configuration must be entirely absent or include access token, webhook secret, and Free/Solo/Teams product IDs",
    );
  }
  if (new Set([input.polarProductFreeId, solo, teams]).size !== 3) {
    return yield* invalid(
      "Free, Solo, and Teams must use distinct Polar product IDs",
    );
  }

  return {
    mode: "hosted",
    accessToken: input.polarAccessToken,
    webhookSecret: input.polarWebhookSecret,
    server: input.polarServer,
    productIds: { free: input.polarProductFreeId, solo, teams },
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
      installerUrl: raw.installerUrl,
      installerSha256: raw.installerSha256,
    },
    inngest: {
      eventKey: raw.inngestEventKey,
      signingKey: raw.inngestSigningKey,
      signingKeyFallback: raw.inngestSigningKeyFallback,
    },
    encryptionSecret: raw.encryptionSecret,
  } as const;
});

export class AppConfig extends Context.Service<AppConfig>()("ployz/AppConfig", {
  make: makeAppConfig,
}) {
  static readonly layer = Layer.effect(this, this.make);
}
