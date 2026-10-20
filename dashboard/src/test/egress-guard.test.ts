import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import net from "node:net";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { allowsHost } from "#/test/egress-guard";

// `.invalid` never resolves, so a broken guard fails these with ENOTFOUND instead of reaching anything.
const PUBLIC = "https://api.github.invalid/repos";

const connectError = (options: net.NetConnectOpts) =>
  new Promise<NodeJS.ErrnoException>((resolve, reject) => {
    const socket = net.connect(options);
    socket.once("connect", () => reject(new Error("connected")));
    socket.once("error", resolve);
  });

describe("the test egress guard", () => {
  it.each([
    ["a string URL", () => fetch(PUBLIC)],
    ["a Request", () => fetch(new Request(PUBLIC, { method: "POST" }))],
    ["a URL", () => fetch(new URL(PUBLIC))],
  ])("refuses fetch with %s to a public host", async (_, request) => {
    await expect(request()).rejects.toMatchObject({ cause: { code: "ERR_TEST_EGRESS", message: expect.stringContaining("api.github.invalid") } });
  });

  it("refuses a raw socket to a public host", async () => {
    expect(await connectError({ host: "mail.postshiba.invalid", port: 587 })).toMatchObject({ code: "ERR_TEST_EGRESS" });
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
