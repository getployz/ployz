import { Schema } from "effect";
import { slugifySegment } from "#/utils/slug";

export const OrganizationSlug = Schema.Trim.check(Schema.isNonEmpty());

export const Organization = Schema.Struct({
  id: Schema.String.check(Schema.isUUID()),
  name: Schema.String,
  slug: OrganizationSlug,
  logo: Schema.NullOr(Schema.String),
});
export type Organization = typeof Organization.Type;

export const OrganizationState = Schema.Struct({
  activeOrganization: Schema.NullOr(Organization),
  organizations: Schema.Array(Organization),
});
export type OrganizationState = typeof OrganizationState.Type;

export const SyncOrganizationSlug = Schema.Struct({
  organizationSlug: OrganizationSlug,
});
export type SyncOrganizationSlug = typeof SyncOrganizationSlug.Type;

export interface PersonalOrganizationUser {
  readonly id: string;
  readonly name: string;
  readonly email: string;
}

export function personalOrganizationName(user: PersonalOrganizationUser) {
  const name = user.name.trim() || user.email.split("@")[0] || "Personal";
  return `${name}'s Projects`;
}

export function personalOrganizationBaseSlug(user: PersonalOrganizationUser) {
  const firstName = user.name.trim().split(/\s+/u)[0] ?? "";
  return (
    slugifySegment(firstName) ||
    slugifySegment(user.email.split("@")[0] ?? "") ||
    "organization"
  );
}
