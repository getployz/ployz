import "@tanstack/react-start/server-only";
import type { ConfigQuery, ConfigTrusted, ConfigView } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { githubBranchExists, resolveReadableRepository } from "#/modules/github/readable-repository.server";

/** The parts of a command that name a repository or branch; decode it with this. The Store validates the whole command. */
const Text = Schema.String;
const EnvironmentRef = Schema.Struct({
  project: Schema.optional(Schema.NullOr(Text)),
  environment: Schema.optional(Schema.NullOr(Text)),
});
export const GitCommand = Schema.Union([
  Schema.Struct({
    command: Schema.Literal("create_git_service"),
    name: Text,
    repository: Text,
    branch: Schema.optional(Schema.NullOr(Text)),
  }),
  Schema.Struct({
    command: Schema.Literal("edit"),
    environment: Schema.optional(EnvironmentRef),
    changes: Schema.Array(Schema.Struct({ op: Text, path: Text, value: Schema.optional(Schema.Unknown) })),
  }),
]);
const SourceSettings = Schema.Struct({ repository: Schema.optional(Text), branch: Schema.optional(Text) });

/** What one Service's part of a command asks of GitHub; `stored` means read what the Service has now for the rest. */
type Wanted = { repository?: string; branch?: string; stored: boolean };

function wanted(command: typeof GitCommand.Type) {
  const services = new Map<string, Wanted>();
  if (command.command === "create_git_service") {
    const entry: Wanted = { repository: command.repository, stored: false };
    if (command.branch) entry.branch = command.branch;
    services.set(command.name, entry);
    return services;
  }
  for (const change of command.changes) {
    const [service = "", setting] = change.path.split(".", 2);
    const entry = services.get(service) ?? { stored: true };
    const value = Schema.decodeUnknownOption(Text)(change.value);
    if (change.op === "set" && setting === "repository") entry.repository = Option.getOrUndefined(value) ?? entry.repository;
    else if (change.op === "set" && setting === "branch") entry.branch = Option.getOrUndefined(value) ?? entry.branch;
    else if (change.op === "patch" && setting === undefined) {
      const patch = Option.getOrUndefined(Schema.decodeUnknownOption(SourceSettings)(change.value));
      if (patch?.repository === undefined && patch?.branch === undefined) continue;
      entry.repository = patch.repository ?? entry.repository;
      entry.branch = patch.branch ?? entry.branch;
    } else continue;
    services.set(service, entry);
  }
  return services;
}

/**
 * The evidence the Store needs to accept `command`: for each repository it names, whether the caller's Organization
 * may read it (a member's installation, else public) and which of the branches in play exist. Nothing here is taken
 * from the caller; unreadable repositories and missing branches are left out, so the Store refuses them.
 */
export const gatherGitEvidence = Effect.fn("ConfigStore.gatherGitEvidence")(function* (
  organizationId: string,
  command: typeof GitCommand.Type | undefined,
  read: (query: ConfigQuery) => Promise<ConfigView>,
) {
  const trusted: Pick<ConfigTrusted, "repositories"> = { repositories: [] };
  if (command === undefined) return trusted;
  const environment = command.command === "edit" ? command.environment : undefined;
  const branches = new Map<string, Set<string>>();
  for (const [service, want] of wanted(command)) {
    if (want.stored && (want.repository === undefined || want.branch === undefined)) {
      // An edit keeps the Service's current repository or branch; read them to check the other.
      const view = yield* Effect.tryPromise(() => read({
        query: "environment",
        environment: { project: environment?.project ?? null, environment: environment?.environment ?? null },
        path: service,
        all: false,
      })).pipe(Effect.option);
      const found = Option.getOrUndefined(view);
      const values = found?.view === "environment" ? found.values : undefined;
      const current = Option.getOrUndefined(Schema.decodeUnknownOption(SourceSettings)(values));
      want.repository ??= current?.repository;
      want.branch ??= current?.branch;
    }
    if (want.repository === undefined) continue;
    const key = want.repository.toLowerCase();
    const wantedBranches = branches.get(key) ?? new Set<string>();
    if (want.branch !== undefined) wantedBranches.add(want.branch);
    branches.set(key, wantedBranches);
  }
  for (const [name, wantedBranches] of branches) {
    const repository = yield* resolveReadableRepository(organizationId, name);
    if (repository === null) continue;
    const found: Array<string> = [];
    for (const branch of wantedBranches) {
      if (branch !== repository.defaultBranch && (yield* githubBranchExists(repository, branch))) found.push(branch);
    }
    trusted.repositories.push({
      repository: repository.fullName,
      repository_id: repository.repositoryId,
      access: repository.installationId === null
        ? { type: "public" }
        : { type: "github-installation", installationId: repository.installationId },
      default_branch: repository.defaultBranch,
      branches: found,
    });
  }
  return trusted;
});
