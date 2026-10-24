import { assert, it } from "@effect/vitest";
import { CipherSuite } from "@hpke/core";
import { Chacha20Poly1305 } from "@hpke/chacha20poly1305";
import { DhkemX25519HkdfSha256, HkdfSha256 } from "@hpke/dhkem-x25519";
import type { ConfigCommand, SecretValue } from "@ployz/sdk";
import { createHash, randomUUID } from "node:crypto";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { expect } from "vitest";
import { callStoreAsMember, writeStoreAsMember } from "#/modules/config-store/config-store.server";
import { sealValue } from "#/modules/variables/seal-value";
import type { SealItem } from "#/modules/variables/sealing";
import { asTestDouble } from "#/lib/test-double";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { storeVariableWriter } from "./store-variables";
import type { StoreEdit, useStoreWriter } from "./store-write";

const ORGANIZATION = "00000000-0000-4000-8000-0000000051a7";
const SLUG = "shop";
const production = { project: "shop", environment: "production" };
const PLAINTEXT = "tok_live_5ea515325cb640df";
const suite = new CipherSuite({ kem: new DhkemX25519HkdfSha256(), kdf: new HkdfSha256(), aead: new Chacha20Poly1305() });

it.live("the Store takes a value the browser sealed as its exact ciphertext", () =>
  Effect.gen(function* () {
    const cloud = yield* storeTestCloud();
    yield* Effect.gen(function* () {
      const owner = yield* seedStoreOrganization(ORGANIZATION, { name: "Shop", slug: SLUG });
      const { drizzle } = yield* Database;
      const write = (command: ConfigCommand) => writeStoreAsMember({ userId: owner }, SLUG, command);
      for (const command of [
        { command: "create_project", id: randomUUID(), name: "shop", default_environment: randomUUID() },
        { command: "create_service", id: randomUUID(), environment: production, name: "web", image: "whoami:1" },
      ] satisfies ConfigCommand[]) {
        assert.isTrue((yield* write(command)).ok);
      }
      const read = yield* callStoreAsMember({ userId: owner }, SLUG, { operation: "read", query: { query: "services", environment: production } });
      const listed = read.ok && read.value.view === "services" ? read.value : assert.fail(JSON.stringify(read));
      const service = listed.services.find((one) => one.name === "web") ?? assert.fail("no web");
      const target = { environment: listed.environment.id, service: service.id };

      const pair = yield* Effect.promise(() => suite.kem.generateKeyPair());
      const publicKey = Buffer.from(yield* Effect.promise(() => suite.kem.serializePublicKey(pair.publicKey))).toString("base64");
      // What `useSealing`'s `prepare` makes of a fresh read: each value sealed to its key.
      const prepare = (items: readonly SealItem[]) => Promise.all(items.map(async (item): Promise<SecretValue> => ({
        sealedFor: item.target.environment,
        value: { sealed: await sealValue({ organizationId: ORGANIZATION, publicKey }, item.target, item.key, item.value), scope: { service: item.target.service }, key: item.key },
      })));
      // The Store writer's `edit`, sending what `prepare` makes as the dashboard's server function does.
      const sent: SecretValue[] = [];
      const writer = asTestDouble<ReturnType<typeof useStoreWriter>>()({
        edit: ({ environment, changes, prepare: prepared }: StoreEdit) => ({ isPersisted: { promise: Effect.runPromise(Effect.gen(function* () {
          const ready = prepared ? yield* Effect.promise(prepared) : changes;
          for (const change of ready) if (change.op === "set" && change.path === "web.env.TOKEN") sent.push((change.value as { secret: SecretValue }).secret);
          const result = yield* write({ command: "edit", environment, expect: null, changes: ready }).pipe(Effect.provide(cloud));
          return result.ok ? result.value : assert.fail(JSON.stringify(result));
        })) } }),
      });
      const variables = storeVariableWriter(writer, production, "web", [], new Map(), { prepare, target });
      const working = Effect.map(
        drizzle.execute<{ working: string }>(sql`select working from config_environment where organization_id = ${ORGANIZATION} and name = 'production'`, "objects"),
        (rows) => rows[0]?.working ?? assert.fail("no Working State"),
      );

      yield* Effect.promise(() => variables.create("TOKEN", PLAINTEXT, true, false).isPersisted.promise);
      const ciphertext = (sent[0] as { value: { sealed: string } }).value.sealed;
      const fingerprint = `c1:${createHash("sha256").update(Buffer.from(ciphertext, "base64")).digest("hex")}`;
      const sealed = yield* working;
      expect(sealed).toContain(ciphertext);
      expect(sealed).toContain(fingerprint);
      expect(sealed).not.toContain(PLAINTEXT);
    }).pipe(Effect.provide(cloud));
  }), 60_000);
