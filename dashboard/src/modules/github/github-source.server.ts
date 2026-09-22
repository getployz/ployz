import "@tanstack/react-start/server-only";
import { createReadStream, createWriteStream } from "node:fs";
import { mkdir, mkdtemp, open, realpath, rm, stat, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { Readable, Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import { createGunzip } from "node:zlib";
import { Data, Effect } from "effect";
import * as tar from "tar";
import { GithubApi, resolveGithubBranchHead, resolveGithubRepository } from "./github-observation.api";
import { getCachedGithubRepositoryForOrganization } from "./github.repository";

export class GithubSourceError extends Data.TaggedError("GithubSourceError")<{
  readonly message: string;
}> { readonly publicErrorCategory = "validation" as const; }

type SourceIdentity = { organizationId: string; installationId: number | null; repositoryId: number };
const authorizeRepository = Effect.fn("Github.authorizeSourceRepository")(function* (input: SourceIdentity) {
  if (input.installationId !== null && !(yield* getCachedGithubRepositoryForOrganization({ ...input, installationId: input.installationId }))) {
    return yield* new GithubSourceError({ message: "Repository is not connected to this Organization." });
  }
  return yield* resolveGithubRepository(input.installationId, input.repositoryId);
});

export const resolveGithubSourceSha = Effect.fn("Github.resolveSourceSha")(function* (input: SourceIdentity & { branch: string }) {
  const repository = yield* authorizeRepository(input);
  const result = yield* resolveGithubBranchHead(input.installationId, repository, `refs/heads/${input.branch}`);
  if (result.state === "absent") return yield* new GithubSourceError({ message: "The source branch no longer exists." });
  return result.headSha;
});

const SOURCE_LIMITS = { downloadBytes: 256 * 1024 * 1024, expandedBytes: 2 * 1024 * 1024 * 1024, entries: 100_000 };
function byteLimit(max: number) {
  let total = 0;
  return new Transform({ transform(chunk: Buffer, _encoding, done) {
    total += chunk.length;
    done(total > max ? new GithubSourceError({ message: "Repository archive exceeds the source size limit." }) : null, chunk);
  } });
}
function safePath(value: string) {
  if (!value || value.includes("\\") || value.includes("\0") || path.posix.isAbsolute(value) || value.split("/").includes("..")) {
    throw new GithubSourceError({ message: "Repository archive contains an unsafe path." });
  }
  return path.posix.normalize(value).replace(/\/$/, "");
}
function contained(root: string, candidate: string) {
  const relative = path.relative(root, candidate);
  return relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

// The source is streamed to disk. Validate the complete archive before extraction,
// including entries preceding a later symlink, rather than trusting archive order.
export async function extractGithubSource(response: Response, directory: string, signal: AbortSignal,
  limits = SOURCE_LIMITS) {
  if (!response.body) throw new GithubSourceError({ message: "Repository archive is empty." });
  const archive = path.join(directory, "source.tar");
  const reader = response.body.getReader();
  const cancel = () => { void reader.cancel().catch(() => {}); };
  signal.addEventListener("abort", cancel, { once: true });
  async function* chunks() {
    try {
      while (true) {
        signal.throwIfAborted();
        const next = await reader.read();
        if (next.done) return;
        yield next.value;
      }
    } finally { signal.removeEventListener("abort", cancel); await reader.cancel(); reader.releaseLock(); }
  }
  await pipeline(Readable.from(chunks()), byteLimit(limits.downloadBytes), createGunzip(),
    byteLimit(limits.expandedBytes), createWriteStream(archive, { flags: "wx", mode: 0o600 }), { signal });
  const entries = new Map<string, { type: string; link: string }>();
  let invalid: GithubSourceError | undefined;
  const parser = new tar.Parser({ strict: true, onReadEntry(entry) {
    try {
      signal.throwIfAborted();
      const name = safePath(entry.path);
      if (entries.size >= limits.entries || entries.has(name)) throw new GithubSourceError({ message: "Repository archive has too many or duplicate entries." });
      if (!["File", "Directory", "SymbolicLink"].includes(entry.type)) throw new GithubSourceError({ message: "Repository archive contains an unsupported entry." });
      const link = entry.type === "SymbolicLink" ? (entry.linkpath ?? "") : "";
      if (link && (path.posix.isAbsolute(link) || link.includes("\\") || link.includes("\0"))) throw new GithubSourceError({ message: "Repository archive contains an unsafe link." });
      entries.set(name, { type: entry.type, link });
    } catch (error) { invalid = error instanceof GithubSourceError ? error : new GithubSourceError({ message: "Source acquisition cancelled." }); }
    entry.resume();
  } });
  await pipeline(createReadStream(archive), parser, { signal });
  if (invalid) throw invalid;
  const roots = new Set([...entries.keys()].map((name) => name.split("/")[0]));
  if (roots.size !== 1) throw new GithubSourceError({ message: "Repository archive must have one root." });
  const root = [...roots][0];
  if (!root) throw new GithubSourceError({ message: "Repository archive is empty." });
  for (const [name, entry] of entries) {
    const ancestors = name.split("/");
    ancestors.pop();
    while (ancestors.length) {
      if (entries.get(ancestors.join("/"))?.type === "SymbolicLink") throw new GithubSourceError({ message: "Repository archive writes through a symbolic link." });
      ancestors.pop();
    }
    if (entry.type === "SymbolicLink") {
      // Resolve links before processing '..', exactly as the filesystem does.
      // Lexical normalization would miss escapes through another symlink.
      const remaining = name.split("/");
      const resolved: string[] = [];
      let links = 0;
      while (remaining.length) {
        const component = remaining.shift();
        if (!component || component === ".") continue;
        if (component === "..") {
          if (resolved.length <= 1) throw new GithubSourceError({ message: "Repository symbolic link escapes its root." });
          resolved.pop();
          continue;
        }
        resolved.push(component);
        const target = entries.get(resolved.join("/"));
        if (target?.type === "SymbolicLink") {
          if (++links > 40) throw new GithubSourceError({ message: "Repository contains cyclic or excessively nested symbolic links." });
          resolved.pop();
          remaining.unshift(...target.link.split("/"));
        }
      }
    }
    if (path.posix.basename(name) === ".gitmodules") throw new GithubSourceError({ message: "Git submodules are not supported for Cloud builds." });
  }
  const unpack = path.join(directory, "checkout");
  await mkdir(unpack, { mode: 0o700 });
  await pipeline(createReadStream(archive), tar.x({ cwd: unpack, strict: true, noChmod: true, noMtime: true, filter: (_name, entry) => !("type" in entry) || entry.type !== "SymbolicLink" }), { signal });
  // Install validated links last; tar intentionally refuses even safe chained links.
  for (const [name, entry] of entries) {
    signal.throwIfAborted();
    if (entry.type !== "SymbolicLink") continue;
    const destination = path.join(unpack, name);
    await mkdir(path.dirname(destination), { recursive: true, mode: 0o700 });
    await symlink(entry.link, destination);
  }
  await rm(archive);
  for (const [name, entry] of entries) {
    signal.throwIfAborted();
    if (entry.type !== "File") continue;
    const file = await open(path.join(unpack, name), "r");
    try {
      const prefix = Buffer.alloc(128);
      const { bytesRead } = await file.read(prefix, 0, prefix.length, 0);
      if (prefix.subarray(0, bytesRead).toString().startsWith("version https://git-lfs.github.com/spec/v1")) {
        throw new GithubSourceError({ message: "Git LFS content is not supported for Cloud builds." });
      }
    } finally { await file.close(); }
  }
  return path.join(unpack, root);
}

export async function resolveSourcePaths(repositoryDirectory: string, rootDir: string, dockerfilePath?: string) {
  const repository = await realpath(repositoryDirectory);
  const rootDirectory = await realpath(path.resolve(repository, rootDir.replace(/^\/+/, "") || "."));
  if (!contained(repository, rootDirectory) || !(await stat(rootDirectory)).isDirectory()) throw new GithubSourceError({ message: "Build root must be a directory inside the repository." });
  if (dockerfilePath === undefined) return { repositoryDirectory: repository, rootDirectory };
  const dockerfile = await realpath(path.resolve(rootDirectory, dockerfilePath.replace(/^\/+/, "")));
  if (!contained(repository, dockerfile) || !(await stat(dockerfile)).isFile()) throw new GithubSourceError({ message: "Dockerfile must be a file inside the repository." });
  return { repositoryDirectory: repository, rootDirectory, dockerfilePath: dockerfile };
}

export const materializeGithubSource = Effect.fn("Github.materializeSource")(function* (input: SourceIdentity & {
  sha: string; rootDir: string; dockerfilePath?: string;
}) {
  const repository = yield* authorizeRepository(input);
  const directory = yield* Effect.acquireRelease(
    Effect.tryPromise({ try: () => mkdtemp(path.join(tmpdir(), "ployz-source-")), catch: () => new GithubSourceError({ message: "Could not create source workspace." }) }),
    (directory) => Effect.tryPromise({
      try: () => rm(directory, { recursive: true, force: true }),
      catch: () => new GithubSourceError({ message: "Could not remove source workspace." }),
    }).pipe(Effect.tapError((error) => Effect.logError("Source workspace cleanup failed.", error)), Effect.ignore),
  );
  const api = yield* GithubApi;
  const response = yield* api.archive({ installationId: input.installationId, repository, sha: input.sha });
  return yield* Effect.tryPromise({
    try: async (signal) => resolveSourcePaths(await extractGithubSource(response, directory, signal), input.rootDir, input.dockerfilePath),
    catch: (error) => error instanceof GithubSourceError ? error : new GithubSourceError({ message: "Could not acquire the pinned repository source." }),
  });
});
