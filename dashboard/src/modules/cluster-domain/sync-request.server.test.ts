import type { RuntimeWatchView, ServiceId } from "@ployz/sdk";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { beforeEach, expect, it, vi } from "vitest";
import { clusterDomainSyncTap } from "#/modules/cluster-domain/sync-request.server";
import { InngestClient } from "#/modules/inngest/client";
import { runtimeWatchContainerFixture, runtimeWatchFrameFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";

const inngest = new Inngest({ id: "cluster-domain-sync-tap-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
beforeEach(() => { send.mockClear(); });

const ingress = (...machines: string[]): RuntimeWatchView => runtimeWatchFrameFixture({
  services: [{
    identity: "ployz-system/ingress",
    service_id: "i".repeat(32) as ServiceId,
    containers: machines.map((machine) => runtimeWatchContainerFixture(machine, `ingress-${machine}`)),
    hook_containers: [],
  }],
});

it("asks for a Cluster Domain sync each time the Machines running the Ingress Proxy change", async () => {
  const tap = await Effect.runPromise(clusterDomainSyncTap().pipe(Effect.provideService(InngestClient, inngest)));
  async function* source() {
    yield ingress("m1", "m2");
    yield ingress("m2", "m1");
    yield ingress("m2");
    yield ingress("m2");
    yield ingress("m1", "m2");
  }

  let frames = 0;
  for await (const _ of tap("org-1")(source())) frames += 1;

  expect(frames).toBe(5);
  await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(2));
  expect(send).toHaveBeenCalledWith({ name: "cluster-domain/sync.requested", data: { organizationId: "org-1" } });
});
