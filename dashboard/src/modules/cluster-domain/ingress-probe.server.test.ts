import { createServer, type RequestListener } from "node:http";
import type { AddressInfo } from "node:net";
import { Effect } from "effect";
import { afterEach, describe, expect, it } from "vitest";
import type { MachineId } from "#/db/tables";
import { probeIngress } from "#/modules/cluster-domain/ingress-probe.server";

const machineId = "a".repeat(32) as MachineId;
const servers: ReturnType<typeof createServer>[] = [];

async function listen(handler: RequestListener) {
  const server = createServer(handler);
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  return `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
}

const close = (server: ReturnType<typeof createServer>) => new Promise((resolve) => server.close(resolve));

describe("probeIngress", () => {
  afterEach(async () => {
    await Promise.all(servers.splice(0).map(close));
  });

  it("hears Ployz when the verify path answers with the Machine id", async () => {
    const origin = await listen((request, response) => {
      response.end(request.url === "/.ployz-verify" ? `${machineId}\n` : "");
    });

    expect(await Effect.runPromise(probeIngress(origin, machineId))).toBe("ployz");
  });

  it.each([
    ["a 200 with another body", 200, {}, "Welcome to nginx!"],
    ["another Machine's id", 200, {}, "b".repeat(32)],
    ["a redirect", 301, { location: "https://example.com/" }, ""],
    ["a 404", 404, {}, "Not Found"],
  ])("hears something else on %s", async (_, status, headers, body) => {
    const origin = await listen((_request, response) => {
      response.writeHead(status, headers).end(body);
    });

    expect(await Effect.runPromise(probeIngress(origin, machineId))).toBe("taken");
  });

  it("hears nothing when the port refuses the connection", async () => {
    const origin = await listen(() => {});
    await Promise.all(servers.splice(0).map(close));

    expect(await Effect.runPromise(probeIngress(origin, machineId))).toBe("closed");
  });
});
