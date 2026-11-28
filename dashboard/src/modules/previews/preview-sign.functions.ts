import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { preparePreviewSigning } from "./preview-sign.server";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";

const PrepareInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty()),
  namespace: Schema.String.check(Schema.isMaxLength(256)),
});

/** The Cluster's key a preview rule names, and the passkeys the Servers trust to sign it. */
export const preparePreviewSigningServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(PrepareInput))
  .handler(({ context, data }) => runActor(context, preparePreviewSigning(context.actor, data)));
