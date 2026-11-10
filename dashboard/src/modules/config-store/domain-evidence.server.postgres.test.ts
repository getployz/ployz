import { it } from "@effect/vitest";
import type { ConfigCommand, ConfigQuery } from "@ployz/sdk";
import { Effect } from "effect";
import { expect } from "vitest";
import { organizationClusterDomain } from "#/modules/cluster-domain/tables";
import { gatherDomainEvidence, systemDomainEvidence } from "#/modules/config-store/domain-evidence.server";
import type { StoreCall, StoreRead } from "#/modules/config-store/store.contract";
import { organizationCloudflareIngress } from "#/modules/ingress/tables";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000d701";
const here = { project: null, environment: null };
const unread: StoreRead = () => Promise.reject(new Error("unread"));
const write = (command: ConfigCommand): StoreCall => ({ operation: "write", command });
const read = (query: ConfigQuery): StoreCall => ({ operation: "read", query });

/** Organization `ORGANIZATION` with `acme.ployz.test` as its Hosted name, behind a Cloudflare zone when `zone` names one. */
const hosted = Effect.fn(function* (zone: string | null) {
  const services = yield* storeTestCloud();
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  yield* provided(Effect.gen(function* () {
    yield* seedStoreOrganization(ORGANIZATION);
    const { drizzle } = yield* Database;
    yield* drizzle.insert(organizationClusterDomain).values({
      organizationId: ORGANIZATION, endpoint: "https://dns.example.test/", name: "acme.ployz.test", encryptedToken: {} as never, reservedAt: new Date(), leaseRenewedAt: new Date(),
    });
    if (zone !== null) {
      yield* drizzle.insert(organizationCloudflareIngress).values({ organizationId: ORGANIZATION, zone, tunnelId: "0a1b2c", tunnelName: "ployz-shop", state: "active" });
    }
  }));
  const gathered = (call: StoreCall) => provided(gatherDomainEvidence(ORGANIZATION, call, unread));
  return { gathered, system: provided(systemDomainEvidence(ORGANIZATION)) };
});

it.live("every write, a History preview, and a rule's or review's view carry the Cluster Domain Cloud knows now; other reads carry none", () =>
  Effect.gen(function* () {
    const { gathered, system } = yield* hosted(null);
    const named = { cluster_domain: { name: "acme.ployz.test" } };
    expect(yield* gathered(write({ command: "create_service", id: "00000000-0000-4000-8000-00000000d702", environment: here, name: "api", image: null }))).toMatchObject(named);
    expect(yield* gathered(write({ command: "stage_history", environment: here, revision: 1, action: "restore", version: "v1", accept_overwrite: true }))).toMatchObject(named);
    expect(yield* gathered(write({ command: "discard", environment: here, path: null, version: null }))).toMatchObject(named);
    expect(yield* gathered(write({ command: "undo_sync", sync: "sync-1" }))).toMatchObject(named);
    expect(yield* gathered(read({ query: "history_preview", environment: here, revision: 1, action: "restore" }))).toMatchObject(named);
    // A rule's and a review's specs lower generated domains as a Deploy does, or no Server would match them.
    expect(yield* gathered(read({ query: "rule", environment: here }))).toMatchObject(named);
    expect(yield* gathered(read({ query: "preview_review", environment: here }))).toMatchObject(named);
    expect(yield* system).toMatchObject(named);
    expect(yield* gathered(read({ query: "environment", environment: here, path: null, all: true }))).toMatchObject({ cluster_domain: null });
  }));

it.live("behind a Cloudflare Tunnel a write's Cluster Domain is the zone, not the Hosted name a Deploy ran under", () =>
  Effect.gen(function* () {
    const { gathered, system } = yield* hosted("acme-apps.com");
    const zone = { cluster_domain: { name: "acme-apps.com", status: { kind: "ready" } } };
    expect(yield* gathered(write({ command: "create_service", id: "00000000-0000-4000-8000-00000000d702", environment: here, name: "api", image: null }))).toMatchObject(zone);
    expect(yield* gathered(read({ query: "history_preview", environment: here, revision: 1, action: "restore" }))).toMatchObject(zone);
    expect(yield* system).toMatchObject(zone);
  }));
