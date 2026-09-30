import crypto from "node:crypto";
import { it } from "@effect/vitest";
import type { ConfigCommand } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { callStore, gatherTrusted } from "#/modules/config-store/config-store.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import {
  checkInStoreGithubBuild, planStoreGithubBuilds, recordStoreGithubBuildSteps, startStoreGithubBuild,
} from "#/modules/config-store/store-github-builds.server";
import { GITHUB_OIDC_ISSUER, GithubOidcKeys } from "#/modules/github/github-oidc.server";
import { type ConnectedRuntimeClient, OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { fakeGithubApi } from "#/test/fake-github";
import { storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000b001";
const PROJECT = "00000000-0000-4000-8000-00000000b002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000b003";
const SERVICE = "00000000-0000-4000-8000-00000000b004";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000b101";
const BUILD = `${DEPLOYMENT}.web`;
const HEAD = "c".repeat(40);
const RUN = 9001;
const WORKFLOW = "acme/web/.github/workflows/ployz-build.yml@refs/heads/main";
const here = { project: null, environment: null };

const signing = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
const jwk = { ...signing.publicKey.export({ format: "jwk" }), kid: "key-1" };
type RunnerClaims = { repository_id: string; job_workflow_ref: string; run_id: string; event_name: string };
/** A GitHub Actions OIDC token for the dispatched run, with `claims` overriding any claim. */
function oidcToken(claims: Partial<RunnerClaims> = {}, key: crypto.KeyObject = signing.privateKey) {
  const part = (json: string) => Buffer.from(json).toString("base64url");
  const body = `${part(JSON.stringify({ alg: "RS256", kid: "key-1" }))}.${part(JSON.stringify({
    iss: GITHUB_OIDC_ISSUER, aud: "http://localhost:3000", exp: Math.floor(Date.now() / 1000) + 300,
    repository_id: "42", job_workflow_ref: WORKFLOW, run_id: String(RUN), event_name: "workflow_dispatch", ...claims,
  }))}`;
  return `${body}.${crypto.sign("RSA-SHA256", Buffer.from(body), key).toString("base64url")}`;
}

/** GitHub with `acme/web` through installation 7, without the build workflow. */
const github = fakeGithubApi({
  "https://api.github.com/repositories/42": { id: 42, full_name: "acme/web", default_branch: "main" },
});

it.live(
  "a Store build goes to GitHub first; its check-in must come from the dispatched repository, workflow and run; a cancel ends it",
  () =>
    Effect.gen(function* () {
      const services = yield* Layer.build(yield* storeTestCloud({ github: github.service }));
      const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(
        Effect.provide(services),
        Effect.provideService(GithubOidcKeys, { keys: Effect.succeed([jwk]) }),
      );
      const store = yield* provided(cloudStore);
      const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command, {
        repositories: [{
          repository: "acme/web", repository_id: 42, access: { type: "github-installation", installationId: 7 },
          default_branch: "main", branches: [],
        }],
        domains: { custom_domains: false, cluster_domain: null, certificates: null, ingress_addresses: [], lookups: [] },
      }));
      yield* write({ command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
      yield* write({ command: "create_git_service", id: SERVICE, environment: here, name: "web", repository: "acme/web", branch: null });
      yield* write({ command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [] });
      const view = () => Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: DEPLOYMENT }));

      // Auto tries GitHub first: GitHub gets the pinned build.
      yield* Effect.promise(() => store.pinSources(DEPLOYMENT, { web: HEAD }));
      const data = { organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId: DEPLOYMENT };
      const [target] = yield* provided(planStoreGithubBuilds(data));
      if (target === undefined) return expect.unreachable("GitHub gets the build");
      expect(target).toEqual({
        build: BUILD, service: "web", repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: true,
      });

      // Without the build workflow GitHub is skipped at once, with why, and the servers take it.
      expect(yield* provided(startStoreGithubBuild(ORGANIZATION, target))).toEqual({ kind: "done" });
      expect(yield* view()).toMatchObject({
        builds: [{ service: "web", commit: HEAD, status: "pending", message: "acme/web has no .github/workflows/ployz-build.yml on its default branch" }],
      });
      // A skipped build is no longer GitHub's to plan.
      expect(yield* provided(planStoreGithubBuilds(data))).toEqual([]);

      // A build GitHub holds: only its dispatched run's token checks in.
      yield* write({ command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-00000000b102", environment: here, services: [], version: null, accept_volume_loss: [] });
      const next = `00000000-0000-4000-8000-00000000b102.web`;
      yield* Effect.promise(() => store.pinSources("00000000-0000-4000-8000-00000000b102", { web: HEAD }));
      yield* Effect.promise(() => store.githubDispatched(next, {
        run_id: RUN, run_url: "https://github.com/acme/web/actions/runs/9001", workflow_ref: WORKFLOW, repository: "acme/web", installation_id: 7,
      }));
      const request = (token: string) =>
        new Request(`http://localhost:3000/api/builds/${next}/check-in`, { method: "POST", headers: { authorization: `Bearer ${token}` } });
      const rejection = (token: string) => provided(Effect.flip(checkInStoreGithubBuild(request(token), next)));
      expect(yield* rejection(oidcToken({}, crypto.generateKeyPairSync("rsa", { modulusLength: 2048 }).privateKey))).toMatchObject({ _tag: "Unauthorized" });
      expect(yield* rejection(oidcToken({ repository_id: "43" }))).toMatchObject({ _tag: "Forbidden", message: "The token is for another repository" });
      expect(yield* rejection(oidcToken({ job_workflow_ref: WORKFLOW.replace("main", "evil") })))
        .toMatchObject({ _tag: "Forbidden", message: "The token is for another workflow or branch" });
      expect(yield* rejection(oidcToken({ run_id: "9002" }))).toMatchObject({ _tag: "Forbidden", message: "The token is for another run" });
      expect(yield* rejection(oidcToken({ event_name: "push" }))).toMatchObject({ _tag: "Forbidden", message: "The run was not dispatched by Ployz" });
      // The right run with no Server to mint a grant: the runner retries its check-in.
      expect(yield* rejection(oidcToken())).toMatchObject({ _tag: "BuildGrantUnavailable" });
      // Build Steps before the check-in are refused.
      const steps = yield* provided(Effect.flip(recordStoreGithubBuildSteps(request(oidcToken()), next, JSON.stringify({ from: 0, events: [] }))));
      expect(steps).toMatchObject({ _tag: "Conflict", message: "This build has not checked in" });

      // Cancelling the Deployment ends the build GitHub holds; a late check-in is refused.
      const cancelled = yield* provided(callStore(ORGANIZATION, "00000000-0000-4000-8000-00000000b0ff", {
        operation: "write", command: { command: "cancel", deployment: "00000000-0000-4000-8000-00000000b102" },
      }));
      expect(cancelled).toMatchObject({ ok: true });
      expect(yield* Effect.promise(() => store.githubBuild(next))).toMatchObject({ status: "failed" });
      expect(yield* rejection(oidcToken())).toMatchObject({ _tag: "Conflict" });

      // A walk that starts with the Servers leaves the build to them, unless no Server takes builds: then GitHub.
      yield* write({ command: "set_build_order", build_order: "servers-then-github" });
      const serversFirst = "00000000-0000-4000-8000-00000000b103";
      yield* write({ command: "admit", admit: "deploy", id: serversFirst, environment: here, services: [], version: null, accept_volume_loss: [] });
      yield* Effect.promise(() => store.pinSources(serversFirst, { web: HEAD }));
      const plan = { ...data, deploymentId: serversFirst };
      expect(yield* provided(planStoreGithubBuilds(plan))).toEqual([]);
      const noBuilders = asTestDouble<OrganizationRuntimeService>()({
        cancel: () => Effect.void,
        open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<ConnectedRuntimeClient>()({
          watchFirstFrame: () => Effect.succeed({ machines: [{ machine: { accepts_builds: false } }] }),
        }) }),
      });
      expect(yield* provided(planStoreGithubBuilds(plan).pipe(Effect.provideService(OrganizationRuntime, noBuilders))))
        .toMatchObject([{ build: `${serversFirst}.web`, hasNext: false }]);

      // An admission carries how many Servers could run it: none here.
      const admission = { operation: "write", command: {
        command: "admit", admit: "deploy", id: crypto.randomUUID(), environment: { project: null, environment: null },
        services: [], version: null, accept_volume_loss: [],
      } } as const;
      const trusted = yield* provided(gatherTrusted(ORGANIZATION, admission, (query) => store.read(ORGANIZATION, query)));
      expect(trusted.servers).toBe(0);
    }),
  60_000,
);
