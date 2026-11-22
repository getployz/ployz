import { Effect, Schema } from "effect";
import { callerOrganizations } from "#/modules/identity/caller.server";
import { removeOrganization } from "#/modules/organization-teardown/organization-removal.server";
import { CreateOrganization } from "#/modules/organization/organization-state";
import { createOrganizationForSession } from "#/modules/organization/organization-state.server";
import { approvalCarried, decodeBody, gated, type CliRoutes } from "#/server/cli-call";
import { cliRefusalResponse } from "#/server/public-error";
import { slugifySegment } from "#/utils/slug";

/** The slug defaults to the name; both then decode as the dashboard's New organization form does. */
const NewOrganization = Schema.Struct({ name: Schema.String, slug: Schema.optional(Schema.String) });
const BAD_ORGANIZATION = "An Organization needs a name of 1 to 64 characters, and a slug of 1 to 64 letters, digits or hyphens.";
const NAME_WITHOUT_SLUG = "This name has no letters or digits to make a slug from. Pass --slug.";

/** The Organizations a Caller reads, makes and removes. */
export const organizationCliRoutes = {
  "GET organizations": ({ caller }) =>
    Effect.gen(function* () {
      return { organizations: yield* callerOrganizations(caller), via: caller.via };
    }),
  "POST organizations": ({ caller, request }) =>
    Effect.gen(function* () {
      if (caller.credential.kind !== "session") {
        return cliRefusalResponse({
          code: "invalid_argument",
          message: "Only a signed-in person can make an Organization; this credential acts only in its own. No changes made.",
          details: { next: "ployz login" },
        });
      }
      const body = yield* decodeBody(NewOrganization, request, BAD_ORGANIZATION);
      const input = yield* Schema.decodeUnknownEffect(CreateOrganization)({ name: body.name, slug: body.slug ?? body.name }).pipe(
        Effect.catchIf(Schema.isSchemaError, () => Effect.succeed(null)),
      );
      if (input === null) {
        const unsluggable = body.slug === undefined && body.name.trim() !== "" && slugifySegment(body.name) === "";
        return cliRefusalResponse({ code: "invalid_argument", message: unsluggable ? NAME_WITHOUT_SLUG : BAD_ORGANIZATION, details: null });
      }
      return { organization: yield* createOrganizationForSession(caller.userId, caller.credential.id, input) };
    }),
  "DELETE organizations/:id": ({ caller, request, id }) =>
    Effect.gen(function* () {
      // A Project refuses the removal before the gate, so an approval it carries is checked first.
      const carried = yield* approvalCarried(caller, request, { command: "remove", subject: `organization:${caller.organization.id}` });
      if (carried !== null) return carried;
      const removal = yield* removeOrganization(caller, decodeURIComponent(id ?? ""), { gate: gated(caller, request) });
      return removal.ok ? removal.value : cliRefusalResponse(removal.refusal);
    }),
} satisfies CliRoutes;
