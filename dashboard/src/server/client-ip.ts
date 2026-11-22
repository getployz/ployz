import { BlockList, isIP } from "node:net";
import { Context } from "effect";

/** The client address of the request being served, as the app trusts it behind its proxy; null outside an HTTP request. */
export const ClientAddress = Context.Reference<string | null>("ployz/ClientAddress", { defaultValue: () => null });

const proxyPeers = new BlockList();
for (const [network, prefix] of [["127.0.0.0", 8], ["10.0.0.0", 8], ["172.16.0.0", 12], ["192.168.0.0", 16], ["169.254.0.0", 16]] as const) {
  proxyPeers.addSubnet(network, prefix, "ipv4");
}
for (const [network, prefix] of [["::1", 128], ["fc00::", 7], ["fe80::", 10]] as const) proxyPeers.addSubnet(network, prefix, "ipv6");

function fromProxy(peer: string) {
  const address = peer.startsWith("::ffff:") && isIP(peer.slice(7)) === 4 ? peer.slice(7) : peer;
  return proxyPeers.check(address, isIP(address) === 4 ? "ipv4" : "ipv6");
}

const HOP_WITH_PORT = /^\[([^\]]+)\](?::\d+)?$|^(\d{1,3}(?:\.\d{1,3}){3}):\d+$/u;

/** Some proxies append `ip:port` or `[ipv6]:port`; the port is not part of the address. */
const hopAddress = (hop: string) => {
  const match = HOP_WITH_PORT.exec(hop);
  return match?.[1] ?? match?.[2] ?? hop;
};

/** Proxies append, so only the right end of the chain is trustworthy: the first hop that isn't a proxy is the client. */
function clientFromForwardedFor(value: string | null, peer: string) {
  let client = peer;
  for (const hop of (value ?? "").split(",").map((entry) => hopAddress(entry.trim())).reverse()) {
    if (isIP(hop) === 0) break;
    client = hop;
    if (!fromProxy(hop)) break;
  }
  return client;
}

const valueAddedLast = (headers: Headers, name: string) => headers.get(name)?.split(",").at(-1)?.trim();

export function observedIp(headers: Headers, clientIpHeaders: ReadonlyArray<string> | undefined, peer: string | undefined) {
  const validPeer = peer !== undefined && isIP(peer) !== 0 ? peer : undefined;
  if (clientIpHeaders !== undefined) {
    for (const name of clientIpHeaders) {
      const value = valueAddedLast(headers, name);
      if (value !== undefined && isIP(value) !== 0) return value;
    }
    return validPeer ?? "unknown";
  }
  if (validPeer === undefined) return "unknown";
  return fromProxy(validPeer) ? clientFromForwardedFor(headers.get("x-forwarded-for"), validPeer) : validPeer;
}
