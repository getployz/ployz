import { Schema } from "effect";
import { OrganizationSlug, Uuid } from "#/modules/environment-design/workspace-schemas";
import type { PrShutdown } from "./tables";

/** Shut a PR Environment down, or deploy it again. */
export const OffCommand = Schema.Struct({ organizationSlug: OrganizationSlug, environmentId: Uuid });
export type OffCommand = typeof OffCommand.Type;

/** Shut down runs unless a shutdown is running or it's already Off; a failed one runs again. */
export const canShutDown = (shutdown: PrShutdown | null) => shutdown === null || shutdown === "failed";
