import { Schema } from "effect";

/** Forget Servers: the Organization whose every Server is gone. */
export const ForgetServersInput = Schema.Struct({ organizationSlug: Schema.String });
