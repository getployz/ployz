import { Schema } from "effect";
import { slugifySegment } from "#/utils/slug";

export const Uuid = Schema.String.check(Schema.isUUID());

export const OrganizationSlug = Schema.Trim.check(Schema.isNonEmpty());
export const ProjectSlug = Schema.Trim.check(Schema.isNonEmpty());
export const EnvironmentName = Schema.Trim.check(Schema.isNonEmpty());
export const EnvironmentSlug = Schema.Trim.check(Schema.isNonEmpty());

export const Organization = Schema.Struct({
  id: Uuid,
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

export const Project = Schema.Struct({
  id: Uuid,
  organizationId: Uuid,
  name: Schema.String,
  slug: ProjectSlug,
});
export type Project = typeof Project.Type;

export const Environment = Schema.Struct({
  id: Uuid,
  projectId: Uuid,
  organizationId: Uuid,
  name: EnvironmentName,
  namespace: EnvironmentSlug,
});
export type Environment = typeof Environment.Type;

export const ProjectList = Schema.Struct({
  organizationSlug: OrganizationSlug,
  name: Schema.optionalKey(EnvironmentName),
});
export type ProjectList = typeof ProjectList.Type;

export const ProjectBySlug = Schema.Struct({
  organizationSlug: OrganizationSlug,
  projectSlug: ProjectSlug,
});
export type ProjectBySlug = typeof ProjectBySlug.Type;

export const EnvironmentList = Schema.Struct({
  organizationSlug: OrganizationSlug,
  projectSlug: ProjectSlug,
});
export type EnvironmentList = typeof EnvironmentList.Type;

export const EnvironmentBySlug = Schema.Struct({
  organizationSlug: OrganizationSlug,
  projectSlug: ProjectSlug,
  environmentSlug: EnvironmentSlug,
});
export type EnvironmentBySlug = typeof EnvironmentBySlug.Type;

export const CreateEnvironment = Schema.Struct({
  organizationSlug: OrganizationSlug,
  projectSlug: ProjectSlug,
  name: EnvironmentName,
});
export type CreateEnvironment = typeof CreateEnvironment.Type;

export const DEFAULT_ENVIRONMENT_NAME = "Production";

export function createCanonicalEnvironmentNamespace(input: {
  readonly projectSlug: string;
  readonly environmentName: string;
}) {
  const environmentSlug = slugifySegment(input.environmentName) || "environment";
  return `${input.projectSlug}-${environmentSlug}`;
}

export function projectBaseSlug(name: string) {
  return slugifySegment(name) || "project";
}

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
