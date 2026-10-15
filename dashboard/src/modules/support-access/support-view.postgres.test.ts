import { assert, it } from "@effect/vitest";
import type { ConfigCommand, ConfigQuery, ConfigView, DiffView, EnvironmentView, JsonValue } from "@ployz/sdk";
import { randomUUID } from "node:crypto";
import { Cause, Effect, Exit } from "effect";
import { asTestDouble } from "#/lib/test-double";
import { callStoreAsMember, writeStoreAsMember } from "#/modules/config-store/config-store.server";
import { user } from "#/modules/identity/tables";
import { SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import { openContainerLogs } from "#/modules/runtime/container-logs.server";
import type { SupportAccessChange } from "#/modules/support-access/support-access";
import { setSupportAccess } from "#/modules/support-access/support-access.server";
import { Auth, type AuthService, type AuthSession } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { Forbidden } from "#/server/public-error";
import type { StoreRefusal, ViewAnswer } from "#/modules/config-store/store.contract";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000a003";
const SLUG = "shop";
const production = { project: "shop", environment: "production" };
const SHOWS: ConfigQuery[] = [{ query: "diff", environment: production }, { query: "environment", environment: production, path: null, all: true }];
const answered = (answers: readonly ViewAnswer[]) => {
  const view = (kind: ConfigView["view"]) => answers.flatMap((answer) => answer.result.ok && answer.result.value.view === kind ? [answer.result.value] : [])[0];
  const diff = view("diff");
  const environment = view("environment");
  return { diff: diff?.view === "diff" ? diff : undefined, environment: environment?.view === "environment" ? environment : undefined };
};
const CONTENT = "token: plain-text\n";
const DRAFT_CONTENT = "token: draft-text\n";

/** What one Store answer shows of `api.env.LOG_LEVEL` (plain), `api.env.SECRET_KEY` (sealed) and sentry's `config.yml`. */
type Seen = { readonly plain: JsonValue | undefined; readonly sealed: JsonValue | undefined; readonly file: JsonValue | undefined };

const seenInEnvironment = (view: EnvironmentView): Seen => {
  const at = (path: string) => view.settings.find((row) => row.path === path)?.value;
  return { plain: at("api.env.LOG_LEVEL"), sealed: at("api.env.SECRET_KEY"), file: undefined };
};

const seenInDiff = (view: DiffView): Seen => {
  const settings = view.changes.flatMap((node) => node.settings);
  const at = (path: string) => settings.find((row) => row.path.endsWith(path))?.after;
  return { plain: at("api.env.LOG_LEVEL"), sealed: at("api.env.SECRET_KEY"), file: at(".files.config.yml") };
};

const seenIn = (view: ConfigView): Seen => {
  switch (view.view) {
    case "environment": return seenInEnvironment(view);
    case "diff": return seenInDiff(view);
    case "config": return { plain: undefined, sealed: undefined, file: view.contents["config.yml"] };
    default: return assert.fail(`no values in a ${view.view} view`);
  }
};

const shownWith = (file: boolean): JsonValue => file ? { content: CONTENT, gid: 0, mode: "0444", uid: 0 } : { gid: 0, mode: "0444", uid: 0 };

it.live(
  "support reads values and file contents only as the Organization allows, and members always read them",
  () =>
    Effect.gen(function* () {
      const cloud = yield* storeTestCloud();
      yield* Effect.gen(function* () {
        const owner = yield* seedStoreOrganization(ORGANIZATION, { name: "Shop", slug: SLUG });
        const { drizzle } = yield* Database;
        const [staffRow] = yield* drizzle.insert(user).values({ email: "staff@ployz.test", name: "Staff" }).returning({ id: user.id });
        const staff = staffRow?.id ?? assert.fail("no staff user");
        const asStaff = Effect.provideService(SupportStaff, supportStaffOf([staff]));
        const allow = (change: SupportAccessChange) => setSupportAccess({ userId: owner }, { organizationSlug: SLUG, change }).pipe(asStaff);

        const write = Effect.fn(function* (command: ConfigCommand) {
          const result = yield* callStoreAsMember({ userId: owner }, SLUG, { operation: "write", command }).pipe(asStaff);
          assert.isTrue(result.ok, JSON.stringify(result));
        });
        yield* write({ command: "create_project", id: randomUUID(), name: "shop", default_environment: randomUUID() });
        yield* write({ command: "create_service", id: randomUUID(), environment: production, name: "api", image: "whoami:1" });
        yield* write({ command: "edit", environment: production, expect: null, changes: [
          { op: "set", path: "api.env.LOG_LEVEL", value: "debug" },
          { op: "set", path: "api.env.SECRET_KEY", value: { secret: "hunter2" } },
        ] });
        yield* write({ command: "create_config", id: randomUUID(), environment: production, name: "sentry", mounts: [{ service: "api", dir: "/etc/sentry" }] });
        yield* write({ command: "put_config_file", environment: production, config: "sentry", file: "config.yml", content: CONTENT });

        const queries: ReadonlyArray<ConfigQuery> = [
          { query: "environment", environment: production, path: null, all: true },
          { query: "diff", environment: production },
          { query: "config", environment: production, config: "sentry" },
        ];
        const read = (userId: string) => Effect.forEach(queries, (query) =>
          callStoreAsMember({ userId }, SLUG, { operation: "read", query }).pipe(
            asStaff,
            Effect.map((result) => result.ok ? seenIn(result.value) : assert.fail(JSON.stringify(result))),
          ));
        const expected = (variables: boolean, configs: boolean) => [
          { plain: variables ? "debug" : { secret: true }, sealed: { secret: true }, file: undefined },
          { plain: variables ? "debug" : { secret: true }, sealed: { secret: true }, file: shownWith(configs) },
          { plain: undefined, sealed: undefined, file: configs ? CONTENT : undefined },
        ];

        yield* allow({ setting: "allowed", on: true });
        assert.deepStrictEqual(yield* read(staff), expected(false, false), "support with everything off");
        assert.deepStrictEqual(yield* read(owner), expected(true, true), "a member is never redacted");

        yield* allow({ setting: "variables", on: true });
        assert.deepStrictEqual(yield* read(staff), expected(true, false), "support with variables on");

        yield* allow({ setting: "variables", on: false });
        yield* allow({ setting: "configs", on: true });
        assert.deepStrictEqual(yield* read(staff), expected(false, true), "support with config files on");

        yield* allow({ setting: "configs", on: false });
        const staffWrite = yield* writeStoreAsMember({ userId: staff }, SLUG, {
          command: "edit", environment: production, expect: null, changes: [{ op: "set", path: "api.replicas", value: 2 }],
        }, SHOWS).pipe(asStaff);
        assert.isTrue(staffWrite.ok, "support may still write");
        if (staffWrite.ok) {
          const views = answered(staffWrite.answers ?? []);
          const diff = seenInDiff(views.diff ?? assert.fail("no diff view"));
          assert.deepStrictEqual(diff, { plain: { secret: true }, sealed: { secret: true }, file: shownWith(false) });
          const environment = seenInEnvironment(views.environment ?? assert.fail("no environment view"));
          assert.deepStrictEqual(environment, { plain: { secret: true }, sealed: { secret: true }, file: undefined });
        }
        const ownerWrite = yield* writeStoreAsMember({ userId: owner }, SLUG, {
          command: "edit", environment: production, expect: null, changes: [{ op: "set", path: "api.replicas", value: 3 }],
        }, SHOWS).pipe(asStaff);
        const ownerDiff = ownerWrite.ok ? answered(ownerWrite.answers ?? []).diff : undefined;
        assert.deepStrictEqual(ownerDiff ? seenInDiff(ownerDiff) : null,
          { plain: "debug", sealed: { secret: true }, file: shownWith(true) });

        const published = yield* callStoreAsMember({ userId: owner }, SLUG, { operation: "write", command: { command: "publish", environment: production, version: null } });
        const saved = published.ok && published.value.written === "published" ? published.value.saved : null;
        yield* write({ command: "edit", environment: production, expect: null, changes: [{ op: "set", path: "api.env.LOG_LEVEL", value: "draft-level" }] });
        yield* write({ command: "put_config_file", environment: production, config: "sentry", file: "config.yml", content: DRAFT_CONTENT });
        const reviewed = yield* callStoreAsMember({ userId: owner }, SLUG, { operation: "read", query: { query: "diff", environment: production } });
        const restore = (userId: string) => writeStoreAsMember({ userId }, SLUG, {
          command: "stage_history", environment: production, revision: saved ?? assert.fail(JSON.stringify(published)), action: "restore",
          version: reviewed.ok && reviewed.value.view === "diff" ? reviewed.value.version : assert.fail(JSON.stringify(reviewed)),
        }).pipe(asStaff, Effect.map((result) => result.ok ? assert.fail("restoring over a draft asks first") : result.refusal));
        const values = ["debug", "draft-level", CONTENT, DRAFT_CONTENT];
        const shownIn = (refusal: StoreRefusal) => values.filter((value) => JSON.stringify(refusal).includes(JSON.stringify(value).slice(1, -1)));
        const staffRestore = yield* restore(staff);
        assert.strictEqual(staffRestore.code, "confirmation_required");
        assert.deepStrictEqual(shownIn(staffRestore), [], "a History refusal shows support no values while they're off");
        assert.deepStrictEqual(shownIn(yield* restore(owner)), values, "and shows a member all of them");

        const logsAs = (userId: string, history?: { cursor?: string }) => {
          const session = asTestDouble<AuthSession>()({ user: { id: userId } });
          const auth = asTestDouble<AuthService>()({ getSession: () => Effect.succeed(session) });
          return Effect.exit(openContainerLogs(new Request("http://localhost/api/runtime/logs"), {
            organizationSlug: SLUG, projectSlug: "shop", environmentSlug: "production",
          }, history).pipe(asStaff, Effect.provideService(Auth, auth)));
        };
        const hidden = (exit: Exit.Exit<{ type: string }, unknown>) => Exit.isSuccess(exit) && exit.value.type === "hidden";
        const refused = (exit: Exit.Exit<unknown, unknown>) => Exit.isFailure(exit) && Cause.squash(exit.cause) instanceof Forbidden;
        assert.isTrue(hidden(yield* logsAs(staff)), "support's log stream says container logs are hidden while they're off");
        assert.isTrue(refused(yield* logsAs(staff, {})), "and their stored history is refused");
        yield* allow({ setting: "logs", on: true });
        for (const [who, userId] of [["support once they're on", staff], ["a member, always", owner]] as const) {
          const [stream, history] = [yield* logsAs(userId), yield* logsAs(userId, {})];
          assert.isFalse(hidden(stream) || refused(stream) || refused(history), `container logs reach the runtime for ${who}`);
        }
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);
