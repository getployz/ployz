import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { disconnectGithub } from "#/modules/github/github-cli.server";
import { MAX_OFFERED_INSTALLATIONS } from "#/modules/github/github-connect";
import { acceptGithubOffer, listGithubOffer, listOrganizationGithub } from "#/modules/github/github-connect.server";
import { githubIdSchema } from "#/modules/github/github-ingestion.contracts";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const authenticated = [publicErrorMiddleware, actorMiddleware] as const;
const OrganizationScope = Schema.Struct({
  organizationSlug: Schema.NonEmptyString.check(Schema.isMaxLength(256)),
});
const GithubOffer = Schema.Struct({ ...OrganizationScope.fields, offer: Schema.NonEmptyString.check(Schema.isMaxLength(8192)) });

export const acceptGithubOfferInput = strictValidator(Schema.Struct({
  ...GithubOffer.fields,
  installationIds: Schema.Array(githubIdSchema).check(Schema.isMaxLength(MAX_OFFERED_INSTALLATIONS)),
}));

export const listOrganizationGithubServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(OrganizationScope))
  .handler(({ context, data }) => runActor(context, listOrganizationGithub(context.actor, data.organizationSlug)));

export const disconnectGithubAccountServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({
    ...OrganizationScope.fields,
    login: Schema.NonEmptyString.check(Schema.isMaxLength(39)),
  })))
  .handler(({ context, data }) => runActor(context, disconnectGithub(context.actor, data.organizationSlug, data.login)));

export const listGithubOfferServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(GithubOffer))
  .handler(({ context, data }) => runActor(context, listGithubOffer(context.actor, data.organizationSlug, data.offer)));

export const acceptGithubOfferServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(acceptGithubOfferInput)
  .handler(({ context, data }) => runActor(context, acceptGithubOffer(context.actor, data)));
