import { type AddressInfo, BlockList, connect, createServer, type Server, type Socket } from "node:net";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProbeRequest } from "#/modules/machines/waiting/checkin-wire";
import { type ProbeResults, portStates } from "#/modules/machines/waiting/probe";
import { connectOnce, probeGreeting, probePeers, probeThroughWindow, refusedTarget } from "#/modules/machines/waiting/probe.server";

const servers: Server[] = [];
afterEach(async () => {
  await Promise.all(servers.splice(0).map((server) => new Promise((resolve) => server.close(resolve))));
});

async function listen(onConnection: (socket: Socket) => void) {
  const server = createServer((socket) => {
    socket.on("error", () => {});
    onConnection(socket);
  });
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  return (server.address() as AddressInfo).port;
}

const none = new BlockList();

describe("refusedTarget", () => {
  it.each([
    "0.1.2.3", "10.0.0.5", "100.64.0.1", "127.0.0.1", "169.254.1.1", "172.16.0.1", "172.31.255.255", "192.0.0.8",
    "192.0.2.1", "192.168.1.1", "198.18.0.1", "198.19.255.1", "198.51.100.7", "203.0.113.7", "224.0.0.1", "255.255.255.255",
    "::", "::1", "::ffff:10.0.0.1", "::ffff:127.0.0.1", "::ffff:a00:1", "64:ff9b::1", "100::1", "2001:db8::1", "fc00::1", "fd12::1",
    "fe80::1", "ff02::1",
  ])("refuses %s", (ip) => {
    expect(refusedTarget(ip, none)).toBe("private_range");
  });

  it.each([
    ["fec0::/10 site-local", "fec0::1"],
    ["fec0::/10 site-local, upper end", "feff::1"],
    ["::/96 IPv4-compatible, dotted", "::10.0.0.5"],
    ["::/96 IPv4-compatible, dotted public", "::8.8.8.8"],
    ["::/96 IPv4-compatible, hex", "::808:808"],
    ["::ffff:0:0/96 SIIT, hex", "::ffff:0:a00:5"],
    ["::ffff:0:0/96 SIIT, dotted", "::ffff:0:10.0.0.5"],
    ["2002::/16 6to4", "2002:a00:5::1"],
    ["2001::/32 Teredo", "2001:0:4136:e378::1"],
    ["192.88.99.0/24 6to4 relay", "192.88.99.1"],
    ["2001:10::/28 ORCHID", "2001:10::1"],
    ["3fff::/20 documentation", "3fff::1"],
  ])("refuses %s (%s)", (_range, ip) => {
    expect(refusedTarget(ip, none)).toBe("private_range");
  });

  it.each(["1.1.1.1", "8.8.8.8", "100.128.0.1", "172.32.0.1", "198.20.0.1", "2606:4700::1111", "::ffff:8.8.8.8", "::ffff:808:808"])(
    "probes %s",
    (ip) => {
      expect(refusedTarget(ip, none)).toBeNull();
    },
  );

  it("has no address to probe for a value that is not an IP", () => {
    expect(refusedTarget("", none)).toBe("no_address");
    expect(refusedTarget("example.com", none)).toBe("no_address");
    expect(refusedTarget("1.2.3", none)).toBe("no_address");
  });

  it("has no address to probe for an address with a zone", () => {
    expect(refusedTarget("2606:4700::1%-", none)).toBe("no_address");
    expect(refusedTarget("2606:4700::1%eth0", none)).toBe("no_address");
  });

  it("probes a lab range only when named", () => {
    const allow = new BlockList();
    allow.addSubnet("192.168.122.0", 24, "ipv4");
    expect(refusedTarget("192.168.122.40", allow)).toBeNull();
    expect(refusedTarget("::ffff:192.168.122.40", allow)).toBeNull();
    expect(refusedTarget("192.168.1.1", allow)).toBe("private_range");
  });
});

const NONCE = "WN5PN_dxAm32GdMdb68fzg";

describe("probeGreeting", () => {
  const keyA = "00".repeat(32);
  const keyB = "11".repeat(32);

  it("derives 22 base64url characters from the handout, the host's key and the port", () => {
    expect(probeGreeting("AAAAAAAAAAAAAAAAAAAAAA", keyA, 80)).toBe("l1OVJ_H135Z5twPXDwQwfR");
    expect(probeGreeting("AAAAAAAAAAAAAAAAAAAAAA", keyA, 443)).toBe("0lBKCojY1NDofwLi7YBLnR");
  });

  it("differs per host, per handout and per port", () => {
    expect(probeGreeting(NONCE, keyA, 80)).not.toBe(probeGreeting(NONCE, keyB, 80));
    expect(probeGreeting(NONCE, keyA, 80)).not.toBe(probeGreeting("AAAAAAAAAAAAAAAAAAAAAA", keyA, 80));
    expect(probeGreeting(NONCE, keyA, 80)).not.toBe(probeGreeting(NONCE, keyA, 443));
  });

  it("reads another host's answer at a shared address as another listener", async () => {
    const port = await listen((socket) => socket.end(probeGreeting(NONCE, keyA, 80)));
    expect((await connectOnce("127.0.0.1", port, probeGreeting(NONCE, keyB, 80))).seen).toBe("other_listener");
    expect((await connectOnce("127.0.0.1", port, probeGreeting(NONCE, keyA, 80))).seen).toBe("answered");
  });

  it("reads a visitor relaying the host's port 80 greeting on port 443 as another listener", async () => {
    const hostPort80 = await listen((socket) => socket.end(probeGreeting(NONCE, keyA, 80)));
    let relayed = 0;
    const visitorPort443 = await listen((socket) => {
      const visitor = connect({ host: "127.0.0.1", port: hostPort80 });
      visitor.on("error", () => socket.destroy());
      visitor.once("data", (greeting) => {
        relayed += 1;
        socket.end(greeting);
        visitor.destroy();
      });
    });
    expect((await connectOnce("127.0.0.1", visitorPort443, probeGreeting(NONCE, keyA, 443))).seen).toBe("other_listener");
    expect(relayed).toBe(1);
    expect((await connectOnce("127.0.0.1", hostPort80, probeGreeting(NONCE, keyA, 80))).seen).toBe("answered");
  });
});

describe("connectOnce", () => {
  it("is answered only when the host writes the greeting", async () => {
    const port = await listen((socket) => socket.write(NONCE));
    expect(await connectOnce("127.0.0.1", port, NONCE)).toEqual({ seen: "answered", bytesRead: NONCE.length });
  });

  it("is another listener when the port accepts and sends nothing", async () => {
    let received = 0;
    const port = await listen((socket) => socket.on("data", (chunk) => (received += chunk.length)));
    expect(await connectOnce("127.0.0.1", port, NONCE, { greetingMs: 50 })).toEqual({ seen: "other_listener", bytesRead: 0 });
    expect(received).toBe(0);
  });

  it("is another listener when the port answers with other bytes", async () => {
    const port = await listen((socket) => socket.write("SSH-2.0-OpenSSH_9.6\r\n"));
    expect((await connectOnce("127.0.0.1", port, NONCE)).seen).toBe("other_listener");
  });

  const drip = (greeting: string) => async (socket: Socket) => {
    for (const byte of greeting) {
      if (socket.destroyed) return;
      socket.write(byte);
      await new Promise((resolve) => setTimeout(resolve, 40));
    }
  };

  it("stops at the deadline even while a listener drips the greeting", async () => {
    const port = await listen(drip(NONCE));
    const started = performance.now();
    expect((await connectOnce("127.0.0.1", port, NONCE, { timeoutMs: 150, greetingMs: 5_000 })).seen).toBe("other_listener");
    expect(performance.now() - started).toBeLessThan(400);
  });

  it("stops at the greeting wait even while a listener drips the greeting", async () => {
    const port = await listen(drip(NONCE));
    const started = performance.now();
    expect((await connectOnce("127.0.0.1", port, NONCE, { timeoutMs: 5_000, greetingMs: 150 })).seen).toBe("other_listener");
    expect(performance.now() - started).toBeLessThan(400);
  });

  it("is refused when nothing listens", async () => {
    const port = await listen(() => {});
    await new Promise((resolve) => servers.pop()?.close(resolve));
    expect((await connectOnce("127.0.0.1", port, NONCE)).seen).toBe("refused");
  });

  it("has no answer when nothing answers the connect", async () => {
    expect((await connectOnce("192.0.2.1", 80, NONCE, { timeoutMs: 50 })).seen).toBe("no_answer");
  });

  it("never follows a redirect", async () => {
    let followed = 0;
    const elsewhere = await listen(() => (followed += 1));
    const port = await listen((socket) =>
      socket.end(`HTTP/1.1 301 Moved Permanently\r\nLocation: http://127.0.0.1:${elsewhere}/\r\n\r\n`),
    );
    expect((await connectOnce("127.0.0.1", port, NONCE)).seen).toBe("other_listener");
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(followed).toBe(0);
  });

  it("reads at most 64 bytes of a large greeting", async () => {
    const port = await listen((socket) => socket.write(Buffer.alloc(1024 * 1024, 0x61)));
    const result = await connectOnce("127.0.0.1", port, NONCE);
    expect(result.seen).toBe("other_listener");
    expect(result.bytesRead).toBeGreaterThan(0);
    expect(result.bytesRead).toBeLessThanOrEqual(64);
  });
});

describe("probeThroughWindow", () => {
  const fast = { firstAfterMs: 20, everyMs: 60, timeoutMs: 200, greetingMs: 50 };

  async function freePort() {
    const port = await listen(() => {});
    await new Promise((resolve) => servers.pop()?.close(resolve));
    return port;
  }

  it("is answered when the host binds late in the window", async () => {
    const port = await freePort();
    setTimeout(() => {
      const server = createServer((socket) => socket.end(NONCE));
      servers.push(server);
      server.listen(port, "127.0.0.1");
    }, 200);
    expect(await probeThroughWindow("127.0.0.1", port, NONCE, 600, fast)).toBe("answered");
  });

  it("is refused only when every attempt through the window was refused", async () => {
    const port = await freePort();
    const started = Date.now();
    expect(await probeThroughWindow("127.0.0.1", port, NONCE, 300, fast)).toBe("refused");
    expect(Date.now() - started).toBeGreaterThanOrEqual(200);
  });

  it("has no answer when an attempt goes unanswered", async () => {
    expect(await probeThroughWindow("192.0.2.1", 80, NONCE, 150, fast)).toBe("no_answer");
  });

  it("has no answer when the window ends before the first attempt", async () => {
    expect(await probeThroughWindow("127.0.0.1", await freePort(), NONCE, 10, fast)).toBe("no_answer");
  });

  it("stops without connecting when the loop wakes after the window has ended", async () => {
    const port = await freePort();
    const start = Date.now();
    vi.spyOn(Date, "now").mockReturnValueOnce(start).mockReturnValueOnce(start).mockReturnValueOnce(start + 1_000);
    try {
      expect(await probeThroughWindow("127.0.0.1", port, NONCE, 300, fast)).toBe("no_answer");
    } finally {
      vi.restoreAllMocks();
    }
  });
});

describe("portStates", () => {
  const request: ProbeRequest = { nonce: NONCE, windowMs: 10_000, listen: ["udp:51820", "tcp:80", "tcp:443"], send: [] };
  const handedAt = "2026-10-17T00:00:00.000Z";
  const during = new Date("2026-10-17T00:00:05.000Z");
  const after = new Date("2026-10-17T00:01:00.000Z");
  const host = { heardFrom: [], bindFailed: [] };
  const both = (seen: "pending" | "answered" | "other_listener" | "refused" | "no_answer") => ({ "80": { seen }, "443": { seen } });

  it("is Unknown without a host report even when every connect was refused", () => {
    const probe: ProbeResults = { request, handedAt, address: "8.8.8.8", tcp: both("refused") };
    expect(portStates(probe, during)["80"]).toEqual({ state: "unknown", reason: "pending" });
    expect(portStates(probe, after)["80"]).toEqual({ state: "unknown", reason: "no_host_report" });
  });

  it("is Closed with a host report and every connect refused", () => {
    expect(portStates({ request, handedAt, address: "8.8.8.8", tcp: both("refused"), host }, after))
      .toEqual({ "80": { state: "closed" }, "443": { state: "closed" } });
  });

  it("is Open when the host answered, report or not", () => {
    expect(portStates({ request, handedAt, address: "8.8.8.8", tcp: both("answered") }, during)["443"]).toEqual({ state: "open" });
  });

  it("is Unknown port busy when the host could not bind, even if something answered", () => {
    const busy = { heardFrom: [], bindFailed: ["tcp:80"] };
    expect(portStates({ request, handedAt, address: "8.8.8.8", tcp: both("answered"), host: busy }, after)["80"])
      .toEqual({ state: "unknown", reason: "port_busy" });
  });

  it("is Unknown port busy when the host could not bind", () => {
    const busy = { heardFrom: [], bindFailed: ["tcp:80"] };
    const states = portStates({ request, handedAt, address: "8.8.8.8", tcp: both("refused"), host: busy }, after);
    expect(states["80"]).toEqual({ state: "unknown", reason: "port_busy" });
    expect(states["443"]).toEqual({ state: "closed" });
    expect(portStates({ request, handedAt, address: "8.8.8.8", tcp: both("other_listener"), host: busy }, after)["80"])
      .toEqual({ state: "unknown", reason: "port_busy" });
  });

  it("settles a connect that never published as no answer once the window and grace pass", () => {
    const probe: ProbeResults = { request, handedAt, address: "8.8.8.8", tcp: both("pending"), host };
    expect(portStates(probe, during)["80"]).toEqual({ state: "unknown", reason: "pending" });
    expect(portStates(probe, after)["80"]).toEqual({ state: "unknown", reason: "no_answer" });
  });

  it("stays pending until the request is handed out", () => {
    expect(portStates({ request, tcp: both("pending") }, after)["80"]).toEqual({ state: "unknown", reason: "pending" });
  });

  it("keeps another listener and an unanswered connect Unknown", () => {
    expect(portStates({ request, handedAt, tcp: both("other_listener"), host }, after)["80"])
      .toEqual({ state: "unknown", reason: "other_listener" });
    expect(portStates({ request, handedAt, tcp: both("no_answer"), host }, after)["80"]).toEqual({ state: "unknown", reason: "no_answer" });
  });
});

describe("probePeers", () => {
  const host = (id: string, observedIp: string) => ({ id, observedIp });

  it("sends each host to the next three, never itself", () => {
    const hosts = [host("a", "1.0.0.1"), host("b", "1.0.0.2"), host("c", "1.0.0.3"), host("d", "1.0.0.4"), host("e", "1.0.0.5")];
    const peers = probePeers(hosts);
    expect(peers.get("a")?.map((peer) => peer.target)).toEqual(["1.0.0.2", "1.0.0.3", "1.0.0.4"]);
    expect(peers.get("e")?.map((peer) => peer.target)).toEqual(["1.0.0.1", "1.0.0.2", "1.0.0.3"]);
    for (const { id, observedIp } of hosts) {
      const targets = peers.get(id) ?? [];
      expect(targets.length).toBeLessThanOrEqual(3);
      expect(targets.map((peer) => peer.target)).not.toContain(observedIp);
      expect(targets.every((peer) => peer.port === 51820)).toBe(true);
    }
  });

  it("gives a lone host no peers and skips shared and missing addresses", () => {
    expect(probePeers([host("a", "1.0.0.1")]).get("a")).toEqual([]);
    const peers = probePeers([host("a", "1.0.0.1"), host("b", "1.0.0.1"), host("c", ""), host("d", "1.0.0.4")]);
    expect(peers.get("a")?.map((peer) => peer.target)).toEqual(["1.0.0.4"]);
  });

  it("never sends a host to a peer at a private or loopback address", () => {
    const peers = probePeers([host("a", "1.0.0.1"), host("b", "10.0.0.2"), host("c", "127.0.0.1"), host("d", "1.0.0.4")]);
    expect(peers.get("a")?.map((peer) => peer.target)).toEqual(["1.0.0.4"]);
    expect(peers.get("b")?.map((peer) => peer.target)).toEqual(["1.0.0.4", "1.0.0.1"]);
  });
});
