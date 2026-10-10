import { Effect, Layer } from "effect";
import { expect, it } from "vitest";
import type { JsonValue } from "@ployz/sdk";
import { FILE_BYTES, repositoryFile, repositoryTree, TREE_PATHS } from "#/modules/github/github-cli.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import type { ReadableRepository } from "#/modules/github/readable-repository.server";
import { fakeGithubApi } from "#/test/fake-github";

const web: ReadableRepository = { fullName: "acme/web", repositoryId: 11, installationId: 7, defaultBranch: "main" };
const sha = "b".repeat(40);
const base = "https://api.github.com/repos/acme/web";

function run<A, E>(effect: Effect.Effect<A, E, GithubApi>, answers: Readonly<Record<string, JsonValue>>) {
  return Effect.runPromise(Effect.result(effect).pipe(Effect.provide(Layer.succeed(GithubApi, fakeGithubApi(answers).service))));
}

const tree = (paths: ReadonlyArray<string>, truncated = false) => ({
  [`${base}/commits/main`]: { commit: { tree: { sha } } },
  [`${base}/git/trees/${sha}?recursive=1`]: { truncated, tree: paths.map((path) => ({ path, type: "blob", mode: "100644" })) },
});

const contents = (path: string, answer: JsonValue) => ({ [`${base}/contents/${path}?ref=main`]: answer });

const base64 = (bytes: Uint8Array | string) => Buffer.from(bytes).toString("base64");

it("lists only the files under the named directory, on the default branch", async () => {
  const listed = await run(repositoryTree(web, "/web/", null), tree(["Dockerfile", "web/package.json", "web/src/a.ts", "webhook.ts"]));
  expect(listed._tag === "Success" && listed.success).toEqual(
    { repository: "acme/web", ref: "main", paths: ["web/package.json", "web/src/a.ts"], truncated: false });
});

it("caps the listing and says it was cut short", async () => {
  const paths = Array.from({ length: TREE_PATHS + 1 }, (_, index) => `f${index}`);
  const listed = await run(repositoryTree(web, null, null), tree(paths));
  if (listed._tag !== "Success") throw new Error("expected a listing");
  expect(listed.success.paths).toHaveLength(TREE_PATHS);
  expect(listed.success.truncated).toBe(true);
  const partial = await run(repositoryTree(web, null, null), tree(["Dockerfile"], true));
  expect(partial._tag === "Success" && partial.success.truncated).toBe(true);
});

it("names a ref GitHub doesn't have as not found", async () => {
  const missing = await run(repositoryTree(web, null, "nope"), tree([]));
  expect(missing._tag === "Failure" && missing.failure).toMatchObject({ _tag: "NotFound", message: "No ref nope in acme/web." });
});

it("returns a text file decoded", async () => {
  const read = await run(repositoryFile(web, "Dockerfile", null),
    contents("Dockerfile", { type: "file", size: 12, encoding: "base64", content: base64("FROM alpine\n") }));
  expect(read._tag === "Success" && read.success).toEqual(
    { repository: "acme/web", ref: "main", path: "Dockerfile", size: 12, content: "FROM alpine\n" });
});

it("returns no content, with a note, for a binary or oversized file", async () => {
  const binary = await run(repositoryFile(web, "logo.png", null),
    contents("logo.png", { type: "file", size: 4, encoding: "base64", content: base64(new Uint8Array([0x89, 0x50, 0, 0x47])) }));
  expect(binary._tag === "Success" && binary.success).toMatchObject({ content: null, note: "logo.png is binary; github cat reads text only." });
  const large = await run(repositoryFile(web, "dump.sql", null),
    contents("dump.sql", { type: "file", size: FILE_BYTES + 1, encoding: "none", content: "" }));
  expect(large._tag === "Success" && large.success).toMatchObject({
    content: null, note: `dump.sql is ${FILE_BYTES + 1} bytes, over the 64 KiB github cat reads.`,
  });
});

it("refuses a directory, pointing at github tree", async () => {
  const directory = await run(repositoryFile(web, "src", null), contents("src", [{ path: "src/a.ts" }]));
  expect(directory._tag === "Failure" && directory.failure).toMatchObject({
    _tag: "Validation", message: "src is a directory: list it with github tree.",
  });
  const missing = await run(repositoryFile(web, "nope", null), {});
  expect(missing._tag === "Failure" && missing.failure).toMatchObject({ _tag: "NotFound", message: "No file nope at main in acme/web." });
});
