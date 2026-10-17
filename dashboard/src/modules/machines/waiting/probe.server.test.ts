import { type AddressInfo, BlockList, createServer, type Server, type Socket } from "node:net";
import { afterEach, describe, expect, it } from "vitest";
import { connectOnce, probePeers, refusedTarget } from "#/modules/machines/waiting/probe.server";

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
    "::", "::1", "::ffff:10.0.0.1", "::ffff:127.0.0.1", "::ffff:a00:1","64:ff9b::1", "100::1", "2001:db8::1", "fc00::1", "fd12::1",
    "fe80::1", "ff02::1",
  ])("refuses %s", (ip) => {
    expect(refusedTarget(ip, none)).toBe("private_range");
  });

  it.each(["1.1.1.1", "8.8.8.8", "100.128.0.1", "172.32.0.1", "198.20.0.1", "2606:4700::1111", "::ffff:8.8.8.8"])(
    "probes %s",
    (ip) => {
      expect(refusedTarget(ip, none)).toBeNull();
    },
  );

  it("has no address to probe for a value that is not an IP", () => {
    expect(refusedTarget("", none)).toBe("no_address");
    expect(refusedTarget("example.com", none)).toBe("no_address");
  });

  it("probes a lab range only when named", () => {
    const allow = new BlockList();
    allow.addSubnet("192.168.122.0", 24, "ipv4");
    expect(refusedTarget("192.168.122.40", allow)).toBeNull();
    expect(refusedTarget("::ffff:192.168.122.40", allow)).toBeNull();
    expect(refusedTarget("192.168.1.1", allow)).toBe("private_range");
  });
});

describe("connectOnce", () => {
  it("is open when the port accepts and sends nothing", async () => {
    let received = 0;
    const port = await listen((socket) => socket.on("data", (chunk) => (received += chunk.length)));
    expect(await connectOnce("127.0.0.1", port, { greetingMs: 50 })).toEqual({ state: "open", bytesRead: 0 });
    expect(received).toBe(0);
  });

  it("is closed when nothing listens", async () => {
    const port = await listen(() => {});
    await new Promise((resolve) => servers.pop()?.close(resolve));
    expect((await connectOnce("127.0.0.1", port)).state).toBe("closed");
  });

  it("never follows a redirect", async () => {
    let followed = 0;
    const elsewhere = await listen(() => (followed += 1));
    const port = await listen((socket) =>
      socket.end(`HTTP/1.1 301 Moved Permanently\r\nLocation: http://127.0.0.1:${elsewhere}/\r\n\r\n`),
    );
    const result = await connectOnce("127.0.0.1", port);
    expect(result.state).toBe("open");
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(followed).toBe(0);
  });

  it("reads at most 64 bytes of a large greeting", async () => {
    const port = await listen((socket) => socket.write(Buffer.alloc(1024 * 1024, 0x61)));
    const result = await connectOnce("127.0.0.1", port);
    expect(result.state).toBe("open");
    expect(result.bytesRead).toBeGreaterThan(0);
    expect(result.bytesRead).toBeLessThanOrEqual(64);
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
});
