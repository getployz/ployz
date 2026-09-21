import { afterEach, expect, it } from "vitest";
import { mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { gzipSync } from "node:zlib";
import { Header } from "tar";
import { extractGithubSource, resolveSourcePaths } from "./github-source.server";
const directories: string[] = [];
afterEach(async () => { await Promise.all(directories.splice(0).map((dir) => rm(dir, { recursive: true, force: true }))); });
async function workspace() { const dir = await mkdtemp(path.join(tmpdir(), "source-test-")); directories.push(dir); return dir; }
function archive(entries: { name: string; body?: string; link?: string }[]) {
  const chunks: Buffer[] = [];
  for (const entry of entries) {
    const body = Buffer.from(entry.body ?? "");
    const header = new Header({ path: entry.name, size: body.length, mode: 0o644,
      type: entry.link ? "SymbolicLink" : "File", linkpath: entry.link });
    header.encode();
    if (!header.block) throw new Error("Missing header");
    chunks.push(Buffer.from(header.block), body, Buffer.alloc((512 - body.length % 512) % 512));
  }
  return new Response(gzipSync(Buffer.concat([...chunks, Buffer.alloc(1024)])));
}
it("extracts source and resolves repository-relative monorepo roots", async () => {
  const repo = await extractGithubSource(archive([{ name: "owner-sha/app/Dockerfile", body: "FROM scratch\n" },
    { name: "owner-sha/app/.dockerignore", body: "private\n" },
    { name: "owner-sha/link", link: "app" }, { name: "owner-sha/alias", link: "link" }]), await workspace(), new AbortController().signal);
  const source = await resolveSourcePaths(repo, "/alias", "Dockerfile");
  if (!source.dockerfilePath) throw new Error("Missing Dockerfile");
  expect(await readFile(source.dockerfilePath, "utf8")).toBe("FROM scratch\n");
  expect(await readFile(path.join(source.rootDirectory, ".dockerignore"), "utf8")).toBe("private\n");
});
it.each([
  { entries: [{ name: "root/../../escape", body: "bad" }] },
  { entries: [{ name: "root/link", link: "../../outside" }] },
  { entries: [{ name: "root/link/file", body: "bad" }, { name: "root/link", link: "safe" }] },
  { entries: [{ name: "root/a", link: "." }, { name: "root/b", link: "a/.." },
    { name: "root/c", link: "b/.." }, { name: "root/d", link: "c/.." }, { name: "root/e", link: "d/.." }] },
  { entries: [{ name: "root/a", link: "b" }, { name: "root/b", link: "a" }] },
  { entries: [{ name: "root/.gitmodules", body: "submodule" }] },
  { entries: [{ name: "root/asset", body: "version https://git-lfs.github.com/spec/v1\noid sha256:123" }] },
])("rejects unsafe or unsupported sources: $entries", async ({ entries }) => {
  await expect(extractGithubSource(archive(entries), await workspace(), new AbortController().signal)).rejects.toThrow();
});
it("bounds expanded bytes and entry count, and interrupts a quiet download", async () => {
  await expect(extractGithubSource(archive([{ name: "root/file", body: "x".repeat(4096) }]), await workspace(), new AbortController().signal,
    { downloadBytes: 10000, expandedBytes: 2048, entries: 10 })).rejects.toThrow("size limit");
  await expect(extractGithubSource(archive([{ name: "root/a" }, { name: "root/b" }]), await workspace(), new AbortController().signal,
    { downloadBytes: 10000, expandedBytes: 10000, entries: 1 })).rejects.toThrow("too many");
  const controller = new AbortController();
  const pending = extractGithubSource(new Response(new ReadableStream()), await workspace(), controller.signal);
  controller.abort();
  await expect(pending).rejects.toThrow();
});
it("rejects root and Dockerfile symlink escapes", async () => {
  const dir = await workspace(); const outside = await workspace();
  await writeFile(path.join(outside, "Dockerfile"), "FROM scratch");
  await symlink(outside, path.join(dir, "escape"));
  await expect(resolveSourcePaths(dir, "escape")).rejects.toThrow("inside the repository");
  await expect(resolveSourcePaths(dir, ".", "escape/Dockerfile")).rejects.toThrow("inside the repository");
});
