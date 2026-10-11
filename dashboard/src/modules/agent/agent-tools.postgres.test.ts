import { it } from "@effect/vitest";
import type { JsonValue } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { describe, expect } from "vitest";
import { toolOutcome } from "#/modules/agent/agent-chat.server";
import { BINDINGS } from "#/modules/agent/agent-tools";
import type { Caller } from "#/modules/identity/actor";
import { fakeGithubApi } from "#/test/fake-github";
import { fakeGithubRepositories } from "#/test/github-repositories";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000a701";

/** One tool call the model makes, and what it hears back. */
type Step = { readonly tool: string; readonly input: JsonValue; readonly turn?: string; readonly hears: object };

const repositories = fakeGithubRepositories({
  "acme/web": {
    defaultBranch: "main",
    files: { "Dockerfile": "FROM node:22\n", "src/index.ts": "serve()\n", "src/app/Dockerfile": "FROM nginx\n", "package.json": "{}\n" },
  },
  "acme/api": { defaultBranch: "trunk", files: { "go.mod": "module api\n" } },
});

/** GitHub as the Store checks a Git Service's source: `acme/docs` is public, with a `dev` branch. */
const github = fakeGithubApi({
  "https://api.github.com/repos/acme/docs": { id: 12, full_name: "acme/docs", private: false, default_branch: "main" },
  "https://api.github.com/repos/acme/docs/git/ref/heads%2Fdev": { ref: "refs/heads/dev", object: { type: "commit", sha: "a".repeat(40) } },
});

const invalid = (message: string) => ({ ok: false, refusal: { code: "invalid_argument", message } });

const shop = [{ tool: "project new", input: { name: "shop" }, hears: { ok: true } }] satisfies Step[];

const cases: ReadonlyArray<{ readonly name: string; readonly steps: ReadonlyArray<Step> }> = [
  {
    name: "a Service added to a new Project is the one the next read lists",
    steps: [
      ...shop,
      { tool: "service add", input: { name: "cache", image: "redis:7" }, hears: { ok: true, value: { service: { name: "cache" } } } },
      { tool: "service ls", input: {}, hears: { ok: true, value: { services: [{ name: "cache", change: "create", source: "image" }] } } },
    ],
  },
  {
    name: "a create retried in its turn replays the first instead of adding a second",
    steps: [
      ...shop,
      ...shop,
      { tool: "project ls", input: {}, hears: { ok: true, value: { projects: [{ name: "shop" }] } } },
      { tool: "service add", input: { name: "cache", image: "redis:7" }, hears: { ok: true } },
      { tool: "service add", input: { name: "cache", image: "redis:7" }, hears: { ok: true } },
      { tool: "service ls", input: {}, hears: { ok: true, value: { services: [{ name: "cache" }] } } },
    ],
  },
  {
    name: "a Git Service from a repository Cloud doesn't see is refused, pointing at github connect",
    steps: [
      ...shop,
      {
        tool: "service add",
        input: { name: "web", repo: "acme/web@dev" },
        hears: { ok: false, refusal: { code: "not_found", details: { next: "ployz github connect", setting: "repository" } } },
      },
      { tool: "service ls", input: {}, hears: { ok: true, value: { services: [] } } },
    ],
  },
  {
    name: "a Git Service is added from a public repository at the branch after @",
    steps: [
      ...shop,
      { tool: "service add", input: { name: "docs", repo: "acme/docs@dev" }, hears: { ok: true, value: { service: { name: "docs" } } } },
      { tool: "service ls", input: {}, hears: { ok: true, value: { services: [{ name: "docs", source: "git" }] } } },
      { tool: "get", input: { path: "docs.branch" }, hears: { ok: true, value: { settings: [{ path: "docs.branch", value: "dev" }] } } },
      { tool: "get", input: { path: "docs.repository" }, hears: { ok: true, value: { settings: [{ path: "docs.repository", value: "acme/docs" }] } } },
    ],
  },
  {
    name: "a service add in a later turn creates the Service anew instead of replaying an earlier turn's",
    steps: [
      ...shop,
      { tool: "service add", input: { name: "cache", image: "redis:7" }, hears: { ok: true } },
      { tool: "service rm", input: { service: "cache" }, hears: { ok: true } },
      { tool: "service add", input: { name: "cache", image: "redis:7" }, turn: "run-2", hears: { ok: true } },
      { tool: "service ls", input: {}, hears: { ok: true, value: { services: [{ name: "cache" }] } } },
    ],
  },
  {
    name: "set stages a Setting that get reads back",
    steps: [
      ...shop,
      { tool: "service add", input: { name: "cache", image: "redis:7" }, hears: { ok: true } },
      { tool: "set", input: { assignment: ["cache.replicas=3"] }, hears: { ok: true, value: { staged: ["cache.replicas"] } } },
      { tool: "get", input: { path: "cache.replicas" }, hears: { ok: true, value: { settings: [{ path: "cache.replicas", value: 3 }] } } },
    ],
  },
  {
    name: "a set that expects a stale revision is refused and stages nothing",
    steps: [
      ...shop,
      { tool: "service add", input: { name: "cache", image: "redis:7" }, hears: { ok: true } },
      {
        tool: "set",
        input: { assignment: ["cache.replicas=3"], expect: "1" },
        hears: { ok: false, refusal: { code: "conflict", message: "Working State moved from revision 1 to 2", details: { revision: 2 } } },
      },
      { tool: "get", input: { path: "cache.replicas" }, hears: { ok: true, value: { settings: [{ path: "cache.replicas", value: 1 }] } } },
    ],
  },
  {
    name: "a Config is created, filled and mounted on a Service",
    steps: [
      ...shop,
      { tool: "service add", input: { name: "web", image: "nginx" }, hears: { ok: true } },
      { tool: "config add", input: { name: "nginx" }, hears: { ok: true } },
      {
        tool: "config put",
        input: { config: "nginx", file: "site.conf", content: "server {}\n" },
        hears: { ok: true, value: { config: { files: [{ name: "site.conf", bytes: 10, mode: "0444" }] } } },
      },
      { tool: "config mount", input: { service: "web", config: "nginx", dir: "/etc/nginx/conf.d" }, hears: { ok: true, value: { staged: ["web.configs.nginx"] } } },
      {
        tool: "diff",
        input: {},
        hears: { ok: true, value: { changes: expect.arrayContaining([expect.objectContaining({ type: "config", name: "nginx", lifecycle: "create" })]) } },
      },
    ],
  },
  {
    name: "an Environment is added to a Project and given a branch setup",
    steps: [
      ...shop,
      { tool: "service add", input: { name: "web", image: "nginx" }, hears: { ok: true } },
      { tool: "env new", input: { name: "staging", project: "shop" }, hears: { ok: true } },
      { tool: "env ls", input: { project: "shop" }, hears: { ok: true, value: { environments: [{ name: "production", default: true }, { name: "staging", default: false }] } } },
      {
        tool: "env setup",
        input: { setup: ["web=pnpm db:seed"] },
        hears: { ok: true, value: { environments: [{ name: "production", branch_setup: [{ service: "web", command: "pnpm db:seed" }] }, { name: "staging", branch_setup: [] }] } },
      },
    ],
  },
  {
    name: "a domain is added to a Service, lowercased",
    steps: [
      ...shop,
      { tool: "service add", input: { name: "web", image: "nginx" }, hears: { ok: true } },
      { tool: "domain add", input: { service: "web", host: " App.Example.com ", port: 8080 }, hears: { ok: true } },
      { tool: "domain ls", input: {}, hears: { ok: true, value: { domains: [{ hostname: "app.example.com", port: 8080, service: "web", status: "setting_up" }] } } },
    ],
  },
  {
    name: "a write with no Project to land in is refused by the Store",
    steps: [{
      tool: "service add",
      input: { name: "cache", image: "redis:7" },
      hears: { ok: false, refusal: { code: "not_found", message: "This Organization has no Project yet", details: { next: "ployz project new NAME" } } },
    }],
  },
  {
    name: "a Service that isn't there is refused by name",
    steps: [
      ...shop,
      { tool: "service rm", input: { service: "ghost" }, hears: { ok: false, refusal: { code: "not_found", message: "No Service named ghost in Environment production" } } },
    ],
  },
  {
    name: "input the tool can't parse is invalid_argument, with the reason",
    steps: [
      ...shop,
      { tool: "set", input: { assignment: ["cache.image"] }, hears: invalid("Expected PATH=VALUE, for example web.replicas=3") },
      { tool: "set", input: { assignment: ["a.replicas=1", "A.replicas=2"] }, hears: invalid("a.replicas is given twice; set it once") },
      { tool: "service add", input: { name: "web", repo: "acme/web", image: "nginx" }, hears: invalid("Give image or repo, not both") },
      { tool: "env setup", input: {}, hears: invalid("Give setup SERVICE=COMMAND, or clear") },
      { tool: "set", input: { assignment: ["a.replicas=1"], expect: "4.5" }, hears: invalid("expect is a revision number, for example 4") },
      { tool: "service add", input: { image: "nginx" }, hears: invalid('Missing key\n  at ["name"]') },
      { tool: "service ls", input: {}, hears: { ok: true, value: { services: [] } } },
    ],
  },
  {
    name: "github tree lists a repository's files at its default branch, narrowed by directory or glob",
    steps: [
      {
        tool: "github tree",
        input: { repository: "acme/web" },
        hears: { ok: true, value: { repository: "acme/web", ref: "main", paths: ["Dockerfile", "src/index.ts", "src/app/Dockerfile", "package.json"], truncated: false } },
      },
      { tool: "github tree", input: { repository: "acme/web", path: "src" }, hears: { ok: true, value: { paths: ["src/index.ts", "src/app/Dockerfile"] } } },
      { tool: "github tree", input: { repository: "acme/web", match: "**/Dockerfile" }, hears: { ok: true, value: { paths: ["Dockerfile", "src/app/Dockerfile"] } } },
      { tool: "github tree", input: { repository: "acme/api", ref: "v1" }, hears: { ok: true, value: { ref: "v1", paths: ["go.mod"] } } },
    ],
  },
  {
    name: "github tree on a repository the Organization can't read names the ones it can",
    steps: [{
      tool: "github tree",
      input: { repository: "acme/nope" },
      hears: {
        ok: false,
        refusal: { code: "not_found", message: "No repository by that name that this Organization can read.", details: { valid_children: ["acme/web", "acme/api"] } },
      },
    }],
  },
  {
    name: "github cat reads one file's text, and refuses one that isn't there",
    steps: [
      { tool: "github cat", input: { repository: "acme/web", path: "Dockerfile" }, hears: { ok: true, value: { ref: "main", path: "Dockerfile", content: "FROM node:22\n" } } },
      { tool: "github cat", input: { repository: "acme/web", path: "Procfile" }, hears: { ok: false, refusal: { code: "not_found" } } },
      { tool: "github cat", input: { repository: "acme/web" }, hears: invalid('Missing key\n  at ["path"]') },
    ],
  },
];

/** Cloud with Shop's Config Store fresh, GitHub answering as `github` and `repositories` say, and Ada calling tools. */
const tools = Effect.fn(function* () {
  const services = yield* Layer.build(yield* storeTestCloud({ github: github.service, repositories }));
  const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" } };
  return ({ tool, input, turn = "run-1" }: Step) => {
    const binding = BINDINGS.get(tool);
    if (binding === undefined || binding.kind === "gated") return Effect.die(`${tool} is not an ungated tool`);
    return toolOutcome(caller, binding, input, turn).pipe(Effect.provide(services));
  };
});

describe("agent tools", () => {
  for (const { name, steps } of cases) {
    it.live(name, () =>
      Effect.gen(function* () {
        const call = yield* tools();
        for (const step of steps) {
          const heard = yield* call(step);
          expect(heard, step.tool).toMatchObject(step.hears);
        }
      }));
  }
});
