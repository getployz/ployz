import { Effect } from "effect";
import { StoreRefused } from "#/modules/config-store/store.contract";
import type { GithubRepositories } from "#/modules/github/github-cli.server";
import { basicGlob } from "#/modules/github/github.server";

/** A repository as the fake holds it: its default branch and each file's text. */
export type FakeRepository = { readonly defaultBranch: string; readonly files: Readonly<Record<string, string>> };

const refused = (code: string, message: string, details: { valid_children: string[] } | null = null) =>
  Effect.fail(new StoreRefused({ code, message, details }));

/**
 * Repository reads answered from `repositories`, keyed by `owner/name`, with no network. Every ref reads the same
 * files. An unknown repository refuses as Cloud does, naming the ones there are.
 */
export const fakeGithubRepositories = (repositories: Readonly<Record<string, FakeRepository>>): GithubRepositories["Service"] => {
  const named = (repository: string): Effect.Effect<FakeRepository, StoreRefused> => {
    const found = repositories[repository];
    return found === undefined
      ? refused("not_found", "No repository by that name that this Organization can read.", { valid_children: Object.keys(repositories).slice(0, 10) })
      : Effect.succeed(found);
  };
  return {
    tree: (_caller, query) => named(query.repository).pipe(Effect.map(({ defaultBranch, files }) => {
      const directory = query.path === undefined ? "" : `${query.path.replace(/\/+$/, "")}/`;
      const matcher = query.match === undefined ? null : basicGlob(query.match);
      const paths = Object.keys(files).filter((path) => path.startsWith(directory) && (matcher?.match(path) ?? true));
      return { repository: query.repository, ref: query.ref ?? defaultBranch, paths, truncated: false };
    })),
    file: (_caller, query) => named(query.repository).pipe(Effect.flatMap(({ defaultBranch, files }) => {
      const content = files[query.path];
      const ref = query.ref ?? defaultBranch;
      if (content === undefined) return refused("not_found", `No file ${query.path} at ${ref} in ${query.repository}.`);
      return Effect.succeed({ repository: query.repository, ref, path: query.path, size: Buffer.byteLength(content), content });
    })),
  };
};
