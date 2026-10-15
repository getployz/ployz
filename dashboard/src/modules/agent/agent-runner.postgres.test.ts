import { it } from "@effect/vitest";
import type { ConfigCommand, JsonValue } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { describe, expect } from "vitest";
import { type AgentCaller, runAgentCall } from "#/modules/agent/agent-runner.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { fakeGithubApi } from "#/test/fake-github";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000a701";
const SHOP: ConfigCommand = {
  command: "create_project", id: "00000000-0000-4000-8000-00000000a702", name: "shop", default_environment: "00000000-0000-4000-8000-00000000a703",
};

type Step = { readonly tool: string; readonly input: JsonValue; readonly callKey?: string; readonly hears: object };

/** GitHub as the Store and github_ls read it: `acme/docs` is public, with a `dev` branch and `README.md` and `src/index.md` on `main`. */
const github = fakeGithubApi({
  "/repos/acme/docs": { id: 12, full_name: "acme/docs", private: false, default_branch: "main" },
  "/repos/acme/docs/git/ref/heads%2Fdev": { ref: "refs/heads/dev", object: { type: "commit", sha: "a".repeat(40) } },
  "/repos/acme/docs/commits/main": { commit: { tree: { sha: "b".repeat(40) } } },
  [`/repos/acme/docs/git/trees/${"b".repeat(40)}?recursive=1`]: {
    truncated: false,
    tree: [
      { path: "README.md", type: "blob", mode: "100644" },
      { path: "src", type: "tree", mode: "040000" },
      { path: "src/index.md", type: "blob", mode: "100644" },
    ],
  },
  "/repos/acme/docs/contents/README.md?ref=main": {
    type: "file", size: 7, encoding: "base64", content: Buffer.from("# Docs\n").toString("base64"),
  },
});

const refused = (code: string) => ({ ok: false, refusal: { code } });

const cases: ReadonlyArray<{ readonly name: string; readonly seed?: boolean; readonly steps: ReadonlyArray<Step> }> = [
  {
    name: "a Service added is the one the next read shows",
    steps: [
      { tool: "service_add", input: { name: "cache", image: "redis:7" }, hears: { ok: true } },
      { tool: "environment_show", input: {}, hears: { ok: true, value: { services: [{ name: "cache" }] } } },
    ],
  },
  {
    name: "a retried create replays the first instead of adding a second",
    steps: [
      { tool: "service_add", input: { name: "cache", image: "redis:7" }, callKey: "add", hears: { ok: true } },
      { tool: "service_add", input: { name: "cache", image: "redis:7" }, callKey: "add", hears: { ok: true } },
      { tool: "environment_show", input: {}, hears: { ok: true, value: { services: [{ name: "cache" }] } } },
    ],
  },
  {
    name: "a Service removed and added again by a new call is created anew",
    steps: [
      { tool: "service_add", input: { name: "cache", image: "redis:7" }, hears: { ok: true } },
      { tool: "service_remove", input: { service: "cache" }, hears: { ok: true } },
      { tool: "service_add", input: { name: "cache", image: "redis:7" }, hears: { ok: true } },
      { tool: "environment_show", input: {}, hears: { ok: true, value: { services: [{ name: "cache" }] } } },
    ],
  },
  {
    name: "a Git Service is added from a public repository at its branch",
    steps: [
      { tool: "service_add", input: { name: "docs", repository: "acme/docs", branch: "dev" }, hears: { ok: true } },
      { tool: "service_inspect", input: { service: "docs" }, hears: { ok: true, value: { settings: { repository: "acme/docs", branch: "dev" } } } },
    ],
  },
  {
    name: "service_update stages a Setting that service_inspect reads back",
    steps: [
      { tool: "service_add", input: { name: "cache", image: "redis:7" }, hears: { ok: true } },
      { tool: "service_update", input: { service: "cache", set: { replicas: 3 } }, hears: { ok: true } },
      { tool: "service_inspect", input: { service: "cache" }, hears: { ok: true, value: { settings: { replicas: 3 } } } },
    ],
  },
  {
    name: "a write that expects a stale revision is refused and stages nothing",
    steps: [
      { tool: "service_add", input: { name: "cache", image: "redis:7" }, hears: { ok: true } },
      { tool: "service_update", input: { service: "cache", set: { replicas: 3 }, expect_revision: 1 }, hears: refused("conflict") },
      { tool: "service_inspect", input: { service: "cache" }, hears: { ok: true, value: { settings: { replicas: 1 } } } },
    ],
  },
  {
    name: "a Config is written and mounted on a Service",
    steps: [
      { tool: "service_add", input: { name: "web", image: "nginx" }, hears: { ok: true } },
      { tool: "config_write", input: { config: "nginx", files: { "site.conf": "server {}\n" }, service: "web", path: "/etc/nginx/conf.d" }, hears: { ok: true } },
      { tool: "environment_show", input: {}, hears: { ok: true, value: { configs: ["nginx"] } } },
    ],
  },
  {
    name: "a domain is added to a Service",
    steps: [
      { tool: "service_add", input: { name: "web", image: "nginx" }, hears: { ok: true } },
      { tool: "domain_add", input: { service: "web", hostname: "app.example.com", port: 8080 }, hears: { ok: true } },
      { tool: "environment_show", input: {}, hears: { ok: true, value: { services: [{ name: "web", domains: ["app.example.com"] }] } } },
    ],
  },
  {
    name: "a write with no Project to land in is refused",
    seed: false,
    steps: [{ tool: "service_add", input: { name: "cache", image: "redis:7" }, hears: refused("not_found") }],
  },
  {
    name: "a Service that isn't there is refused by name",
    steps: [{ tool: "service_remove", input: { service: "ghost" }, hears: { ok: false, refusal: { code: "not_found", message: "No Service named ghost in Environment production" } } }],
  },
  {
    name: "input the tool's schema rejects is invalid_argument, and writes nothing",
    steps: [
      { tool: "service_add", input: { image: "nginx" }, hears: refused("invalid_argument") },
      { tool: "service_update", input: { service: "cache", replica: 3 }, hears: refused("invalid_argument") },
      { tool: "environment_show", input: {}, hears: { ok: true, value: { services: [] } } },
    ],
  },
  {
    name: "a repository the Organization can't read is refused not_found",
    steps: [{ tool: "github_ls", input: { repository: "acme/web" }, hears: refused("not_found") }],
  },
  {
    name: "github_ls lists a public repository at its default branch",
    steps: [{
      tool: "github_ls",
      input: { repository: "acme/docs" },
      hears: { ok: true, value: { repository: "acme/docs", ref: "main", entries: [{ path: "README.md", kind: "file" }, { path: "src", kind: "directory" }] } },
    }],
  },
  {
    name: "github_ls reads a top-level file",
    steps: [{
      tool: "github_ls",
      input: { repository: "acme/docs", path: "README.md" },
      hears: { ok: true, value: { repository: "acme/docs", ref: "main", path: "README.md", content: "# Docs\n" } },
    }],
  },
  {
    name: "github_ls refuses a path the repository doesn't have",
    steps: [{
      tool: "github_ls",
      input: { repository: "acme/docs", path: "Dockerfile" },
      hears: { ok: false, refusal: { code: "not_found", message: "No file or directory at that path", next: [{ tool: "github_ls", input: { repository: "acme/docs", ref: "main" } }] } },
    }],
  },
  {
    name: "project_create makes a Project the next read shows",
    seed: false,
    steps: [
      { tool: "project_create", input: { name: "docs" }, hears: { ok: true, value: { at: "docs/production", project: "docs" } } },
      { tool: "environment_show", input: { project: "docs" }, hears: { ok: true, value: { at: "docs/production" } } },
    ],
  },
  {
    name: "a retried project_create replays the first Project",
    seed: false,
    steps: [
      { tool: "project_create", input: { name: "docs" }, callKey: "create", hears: { ok: true } },
      { tool: "project_create", input: { name: "docs" }, callKey: "create", hears: { ok: true, value: { project: "docs" } } },
    ],
  },
  {
    name: "project_create refuses a name another Project has",
    steps: [{ tool: "project_create", input: { name: "shop" }, hears: refused("conflict") }],
  },
];

const tools = Effect.fn(function* (seed: boolean) {
  const services = yield* Layer.build(yield* storeTestCloud({ github: github.service }));
  const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  if (seed) {
    const store = yield* cloudStore.pipe(Effect.provide(services));
    yield* Effect.promise(() => store.write(ORGANIZATION, SHOP));
  }
  const caller: AgentCaller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, role: "owner", support: null, via: "agent" };
  let calls = 0;
  return ({ tool, input, callKey = `call-${++calls}` }: Step) =>
    runAgentCall(caller, { tool, input, key: `run-1:${callKey}`, approvalId: null, page: { at: "here", project: "shop", environment: "production" } }).pipe(
      Effect.map((result) => "asking" in result ? result : result.outcome),
      Effect.provide(services),
    );
});

describe("agent runner", () => {
  for (const { name, seed = true, steps } of cases) {
    it.live(name, () =>
      Effect.gen(function* () {
        const call = yield* tools(seed);
        for (const step of steps) {
          const heard = yield* call(step);
          expect(heard, step.tool).toMatchObject(step.hears);
        }
      }));
  }
});
