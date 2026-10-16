import { Schema } from "effect";

export const FLEET_TOKEN_PREFIX = "pflt_";

export const FLEET_TOKEN_LIFETIMES = ["1d", "7d", "30d"] as const;
export type FleetTokenLifetime = (typeof FLEET_TOKEN_LIFETIMES)[number];

const OrganizationSlug = Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty());

/** Lowercase letters, digits and hyphens, so a name reads the same in a shell as in the dashboard. */
export const FleetTokenName = Schema.String.check(Schema.isPattern(/^[a-z0-9][a-z0-9-]{0,62}$/));

export const CreateFleetTokenInput = Schema.Struct({
  organizationSlug: OrganizationSlug,
  name: Schema.NullOr(FleetTokenName),
  expiresIn: Schema.Literals(FLEET_TOKEN_LIFETIMES),
  autoEnroll: Schema.NullOr(Schema.Struct({ maxServers: Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 1000 })) })),
});
export type CreateFleetTokenInput = typeof CreateFleetTokenInput.Type;

export const ListFleetTokensInput = Schema.Struct({ organizationSlug: OrganizationSlug });
export type ListFleetTokensInput = typeof ListFleetTokensInput.Type;

export const RevokeFleetTokenInput = Schema.Struct({
  organizationSlug: OrganizationSlug,
  id: Schema.String.check(Schema.isUUID()),
});
export type RevokeFleetTokenInput = typeof RevokeFleetTokenInput.Type;

export type FleetTokenView = {
  readonly id: string;
  readonly name: string;
  readonly expiresAt: string;
  readonly expired: boolean;
  readonly autoEnrollMax: number | null;
  readonly lastUsedAt: string | null;
  readonly createdAt: string;
};
