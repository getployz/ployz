// Seals production's `api.env.SECRET_KEY` to the Cluster's Sealing Key once real Servers have enrolled. seed.ts seals
// its own values as it writes them, but with SERVERS no Cluster has a key until the Machines attach, after the seed.
// up.sh runs this then, through runner.mjs.
import { eq } from "drizzle-orm";
import { Effect, Schedule } from "effect";
import type { Change, ClientSealedValue, EnvironmentRef } from "@ployz/sdk";
import { callStore } from "#/modules/config-store/config-store.server";
import { member, user } from "#/modules/identity/tables";
import { permissionsOf } from "#/modules/organization/permissions";
import { readClusterSealing } from "#/modules/sealing/cluster-sealing.server";
import { sealValue } from "#/modules/variables/seal-value";
import { Database } from "#/server/database.server";
import { AppRuntime } from "#/server/runtime.server";

/** `value` sealed for variable `key` of `service` in `environment` to the Cluster's Sealing Key, or null while the Cluster has none. */
export const sealed = Effect.fn(function* (
  organizationId: string, userId: string, environment: EnvironmentRef, service: string, key: string, value: string,
) {
  const sealing = yield* readClusterSealing(organizationId);
  if (sealing.state !== "ready") return null;
  const read = yield* callStore(organizationId, userId, { operation: "read", query: { query: "services", environment } });
  const listed = read.ok && read.value.view === "services" ? read.value : yield* Effect.die(`no services view: ${JSON.stringify(read)}`);
  const id = listed.services.find((one) => one.name === service)?.id ?? (yield* Effect.die(`no Service ${service}`));
  const target = { environment: listed.environment.id, service: id };
  const ciphertext = yield* Effect.promise(() => sealValue({ organizationId, publicKey: sealing.publicKey }, target, key, value));
  return { secret: { sealedFor: target.environment, value: { sealed: ciphertext, scope: { service: id }, key } } satisfies ClientSealedValue };
});

/** production's exported `api.env.SECRET_KEY`, or null while the Cluster has no key. */
export const secretKey = Effect.fn(function* (organizationId: string, userId: string) {
  const value = yield* sealed(organizationId, userId, { project: "shop", environment: "production" }, "api", "SECRET_KEY", "verify-secret-key");
  if (value === null) return null;
  return [
    { op: "set", path: "api.env.SECRET_KEY", value },
    { op: "set", path: "api.env.SECRET_KEY.exported", value: true },
  ] satisfies Change[];
});

const sealAfterEnrollment = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [owner] = yield* drizzle.select({ userId: user.id, organizationId: member.organizationId }).from(user)
    .innerJoin(member, eq(member.userId, user.id)).where(eq(user.email, "ada@example.com"));
  if (!owner) return yield* Effect.die("seed.ts has not run");
  const changes = (yield* secretKey(owner.organizationId, owner.userId).pipe(Effect.repeat({
    schedule: Schedule.spaced("2 seconds").pipe(Schedule.upTo({ duration: "1 minute" })),
    while: (ready) => ready === null,
  }))) ?? (yield* Effect.die("no Server reported the Cluster's Sealing Key within a minute"));
  const written = yield* callStore(owner.organizationId, owner.userId, {
    operation: "write", command: { command: "edit", environment: { project: "shop", environment: "production" }, expect: null, changes },
  }, { source: "dashboard" }, { approval: "not_required", permissions: permissionsOf("owner") });
  if (!written.ok) return yield* Effect.die(`SECRET_KEY refused: ${written.refusal.code} ${written.refusal.message}`);
});

export async function run() {
  await AppRuntime.runPromise(sealAfterEnrollment);
  console.log("VERIFY_SEED_SEALED api.env.SECRET_KEY");
}
