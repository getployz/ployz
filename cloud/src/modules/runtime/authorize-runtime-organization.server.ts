import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { Auth } from "#/server/auth.server";
import { Unauthorized } from "#/server/public-error";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";

export const authorizeRuntimeOrganization = Effect.fn(
  "Runtime.authorizeOrganization",
)(function* (input: {
  headers: Headers;
  organizationSlug: string;
}) {
  const auth = yield* Auth;
  const session = yield* auth.getSession(input.headers);
  const user = session?.user;
  if (!user) {
    return yield* new Unauthorized();
  }

  const organization = yield* requireInfrastructureOrganization(
    { userId: user.id },
    input.organizationSlug,
  );
  return { organizationId: organization.id };
});
