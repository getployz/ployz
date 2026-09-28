import { Schema } from "effect";
import { OrganizationSlug, Uuid } from "#/modules/environment-design/workspace-schemas";

/** Shut a PR Environment down, or deploy it again. */
export const OffCommand = Schema.Struct({ organizationSlug: OrganizationSlug, environmentId: Uuid });
export type OffCommand = typeof OffCommand.Type;
