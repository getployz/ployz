import type { EventEmitter } from "node:events";
import { mkdtempSync, rmSync } from "node:fs";
import http, { createServer, type Server } from "node:http";
import https from "node:https";
import type { AddressInfo } from "node:net";
import net from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import tls from "node:tls";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { allowsHost } from "#/test/egress-guard";

// `.invalid` never resolves, so a broken guard fails these with ENOTFOUND instead of reaching anything.
const PUBLIC = "https://api.github.invalid/repos";

const egressError = (emitter: EventEmitter) =>
  new Promise<NodeJS.ErrnoException>((resolve, reject) => {
    emitter.once("connect", () => reject(new Error("connected")));
    emitter.once("response", () => reject(new Error("answered")));
    emitter.once("error", resolve);
  });

const answer = (request: http.ClientRequest) =>
  new Promise<string>((resolve, reject) => {
    request.once("error", reject).once("response", (response) => {
      let body = "";
      response.setEncoding("utf8").on("data", (chunk: string) => (body += chunk)).once("end", () => resolve(body));
    });
  });

describe("the test egress guard", () => {
  it.each([
    ["a string URL", () => fetch(PUBLIC)],
    ["a Request", () => fetch(new Request(PUBLIC, { method: "POST" }))],
    ["a URL", () => fetch(new URL(PUBLIC))],
  ])("refuses fetch with %s to a public host", async (_, request) => {
    await expect(request()).rejects.toMatchObject({ cause: { code: "ERR_TEST_EGRESS", message: expect.stringContaining("api.github.invalid") } });
  });

  it.each([
    ["http.get", () => http.get("http://api.github.invalid/repos")],
    ["https.get", () => https.get("https://api.github.invalid/repos")],
    ["tls.connect", () => tls.connect({ host: "api.github.invalid", port: 443 })],
    ["net.connect with options", () => net.connect({ host: "mail.postshiba.invalid", port: 587 })],
    ["net.connect(port, host)", () => net.connect(587, "mail.postshiba.invalid")],
    ["Socket#connect(port, host)", () => new net.Socket().connect(587, "mail.postshiba.invalid")],
  ])("refuses %s to a public host", async (_, open) => {
    expect(await egressError(open())).toMatchObject({ code: "ERR_TEST_EGRESS", message: expect.stringContaining(".invalid") });
  });

  it("refuses a connect whose arguments it can't read", async () => {
    // SAFETY: a host Node would never accept, so only the guard can answer.
    const socket = new net.Socket().connect({ host: ["mail.postshiba.invalid"], port: 587 } as never);
    expect(await egressError(socket)).toMatchObject({ code: "ERR_TEST_EGRESS" });
  });

  describe("on loopback", () => {
    let server: Server;
    let port: number;
    beforeAll(async () => {
      server = createServer((_, response) => response.end("ok"));
      await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
      port = (server.address() as AddressInfo).port;
    });
    afterAll(() => new Promise<void>((resolve) => server.close(() => resolve())));

    it.each(["127.0.0.1", "localhost"])("lets fetch reach %s", async (host) => {
      expect(await (await fetch(`http://${host}:${port}/`)).text()).toBe("ok");
    });

    it("lets a raw socket connect", async () => {
      const socket = net.connect(port, "127.0.0.1");
      await new Promise((resolve, reject) => socket.once("connect", resolve).once("error", reject));
      socket.destroy();
    });

    it("lets http.get reach localhost", async () => {
      expect(await answer(http.get(`http://localhost:${port}/`))).toBe("ok");
    });
  });

  describe("on a Unix socket", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "egress-guard-"));
    const socketPath = path.join(dir, "http.sock");
    let server: Server;
    beforeAll(async () => {
      server = createServer((_, response) => response.end("ok"));
      await new Promise<void>((resolve) => server.listen(socketPath, resolve));
    });
    afterAll(async () => {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      rmSync(dir, { recursive: true, force: true });
    });

    it("lets http.get through", async () => {
      expect(await answer(http.get({ socketPath, host: "api.github.invalid", path: "/" }))).toBe("ok");
    });

    it("lets a raw socket connect", async () => {
      const socket = net.connect({ path: socketPath });
      await new Promise((resolve, reject) => socket.once("connect", resolve).once("error", reject));
      socket.destroy();
    });
  });

  it.each(["localhost", "LOCALHOST.", "app.localhost", "127.0.0.1", "127.8.9.10", "::1", "[::1]", "[0:0:0:0:0:0:0:1]"])(
    "allows loopback %s",
    (host) => expect(allowsHost(host, {})).toBe(true),
  );

  it.each([
    "dns.ployz.app", "dns.ployz.app.", "api.github.com", "localhost.example.com", "notlocalhost", "128.0.0.1", "0.0.0.0", "10.0.0.5",
    "[2606:4700::1111]", "fe80::1%lo",
  ])("refuses %s", (host) => expect(allowsHost(host, {})).toBe(false));

  it("lets only a live eval reach the model APIs", () => {
    expect(allowsHost("api.anthropic.com", {})).toBe(false);
    expect(allowsHost("api.anthropic.com", { PLOYZ_EVAL_LIVE: "0" })).toBe(false);
    expect(allowsHost("API.Anthropic.com.", { PLOYZ_EVAL_LIVE: "1" })).toBe(true);
    expect(allowsHost("api.openai.com", { PLOYZ_EVAL_LIVE: "1" })).toBe(true);
    expect(allowsHost("api.github.com", { PLOYZ_EVAL_LIVE: "1" })).toBe(false);
  });
});
