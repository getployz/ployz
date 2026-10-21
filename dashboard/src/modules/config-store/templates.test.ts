import { describe, expect, it } from "vitest";
import type { EnvironmentView, RowId, ServiceListing } from "@ployz/sdk";
import {
  advance, installs, nextServer, place, planInstall, removeServer, render, runSteps, templateById,
  type InstallRecord, type RunStep, type TemplateMember,
} from "./templates";

const postgres = templateById("postgres")!;

const listed = (name: string, version: number): ServiceListing =>
  ({ id: `${name}-id`, row: `${name}:node` as RowId, name, private_dns: name, source: "image", change: null,
    template: { id: "postgres", version } });

const settingsOf = (members: readonly TemplateMember[], edits: Record<string, string> = {}): EnvironmentView => ({
  environment: { id: "env", project: "shop", name: "production", revision: 1 },
  settings: [
    ...members.flatMap((member) => Object.entries({ image: member.image, ...member.set })
      .map(([field, value]) => ({ path: `${member.name}.${field}`, value: edits[`${member.name}.${field}`] ?? value, default: null, apply: "staged" as const }))),
  ],
});

/** An Install of Postgres v1 named `db`, its password minted as `pw`. */
function v1(): InstallRecord {
  const rendered = render(postgres, 1, {}, "db", { password: "pw" });
  return {
    install: { name: "db", template: "postgres", version: 1, inputs: {}, distinctMachines: false },
    receipt: { version: 1, members: rendered.members, generated: { password: "pw" } },
    decisions: {}, placement: {}, run: null,
  };
}

const viewOf = (record: InstallRecord, settings: EnvironmentView, version = record.install.version) => {
  const services = record.receipt.members.map((member) => listed(member.name, version));
  const [view] = installs(services, settings, { [record.install.name]: record });
  if (!view) throw new Error("no install");
  return { view, services };
};

describe("planInstall", () => {
  it("updates rows nobody changed, keeps edits, and asks about an edited row the release drops", () => {
    const record = v1();
    const settings = settingsOf(record.receipt.members, { "db.env.POSTGRES_DB": "shop", "db.env.POSTGRES_HOST_AUTH_METHOD": "md5" });
    const { view, services } = viewOf(record, settings);
    expect(view.edited.map((row) => [row.path, row.origin])).toEqual([
      ["db.env.POSTGRES_DB", "here"], ["db.env.POSTGRES_HOST_AUTH_METHOD", "here"],
    ]);

    const plan = planInstall(view, 2, view.install.inputs, services, settings);
    expect(plan.changes.map((row) => [row.path, row.target])).toEqual([
      ["db.image", "postgres:17-alpine"], ["db.env.POSTGRES_INITDB_ARGS", "--data-checksums"],
    ]);
    expect(plan.kept.map((row) => row.path)).toEqual(["db.env.POSTGRES_DB"]);
    expect(plan.decide.map((row) => row.path)).toEqual(["db.env.POSTGRES_HOST_AUTH_METHOD"]);
    expect(plan.record.decisions).toEqual({ "db.env.POSTGRES_HOST_AUTH_METHOD": null });
    // The password is minted once: the new base renders the same one, so no row changes it.
    expect(plan.record.receipt.members[0]?.set["env.POSTGRES_PASSWORD"]).toBe("pw");
    expect(plan.changes.some((row) => row.field === "env.POSTGRES_PASSWORD")).toBe(false);

    const after = viewOf(plan.record, settingsOf(plan.record.receipt.members, { "db.env.POSTGRES_DB": "shop", "db.env.POSTGRES_HOST_AUTH_METHOD": "md5" }), 2);
    expect(after.view.toFix).toBe(1);
    const decided = viewOf({ ...plan.record, decisions: { "db.env.POSTGRES_HOST_AUTH_METHOD": "keep" } }, settings, 2);
    expect(decided.view.toFix).toBe(0);
  });

  it("turns single into HA as five members spread over distinct servers", () => {
    const record = v1();
    const settings = settingsOf(record.receipt.members);
    const { view, services } = viewOf(record, settings);
    const plan = planInstall(view, 1, { mode: { state: "value", value: "ha" } }, services, settings);
    expect(plan.added.map((member) => member.name)).toEqual(["db-2", "db-3", "db-etcd", "db-router"]);
    expect(plan.removed).toEqual([]);
    expect(plan.record.install.distinctMachines).toBe(true);
  });
});

describe("place", () => {
  const servers = (count: number, offline: string[] = []) =>
    Array.from({ length: count }, (_, index) => ({ name: `m${index + 1}`, online: !offline.includes(`m${index + 1}`) }));
  const names = ["db", "db-2", "db-3", "db-etcd", "db-router"];

  it("refuses a spread that does not fit before anything moves", () => {
    expect(place(names, servers(3), {}, true)).toEqual({ ok: false, needs: 5, available: 3 });
    expect(place(names, servers(5, ["m4"]), {}, true)).toEqual({ ok: false, needs: 5, available: 4 });
  });

  it("names a new server so it never shares a name after one is removed", () => {
    expect(nextServer([{ name: "m1", online: true }, { name: "m2", online: true }, { name: "m4", online: true }, { name: "m5", online: true }]).name).toBe("m3");
    expect(nextServer(servers(3)).name).toBe("m4");
  });

  it("puts every member on its own server and keeps a placed one where it is", () => {
    const placed = place(names, servers(5), { db: "m3" }, true);
    if (!placed.ok) throw new Error("refused");
    expect(placed.placement["db"]).toBe("m3");
    expect(new Set(Object.values(placed.placement)).size).toBe(5);
  });
});

describe("a run", () => {
  const states = (steps: readonly RunStep[]) => steps.map((step) => [step.label, step.state]);
  const all = (count: number) => Array.from({ length: count }, (_, index) => ({ name: `m${index + 1}`, online: true }));

  it("runs the basebackup Jobs beside the next Deploy instead of holding it", () => {
    const placement = { db: "m1", "db-2": "m2", "db-3": "m3", "db-etcd": "m4", "db-router": "m5" };
    let steps = advance(runSteps(["db"], ["db", "db-2", "db-3", "db-etcd", "db-router"], "db", placement), all(5));
    steps = advance(advance(steps, all(5)), all(5));
    expect(states(steps)).toEqual([
      ["Deploy db-etcd", "done"],
      ["Deploy db", "done"],
      ["Basebackup db-2", "running"],
      ["Basebackup db-3", "running"],
      ["Deploy db-router", "running"],
      ["Deploy db-2, db-3", "queued"],
    ]);
    expect(steps.filter((step) => step.kind === "job").every((step) => step.beside)).toBe(true);
  });

  it("leaves a former replica on an offline server waiting, with no timeout, until that server is removed", () => {
    const placement = { db: "m1", "db-2": "m2", "db-3": "m3", "db-etcd": "m4", "db-router": "m5" };
    const servers = [...all(5)].map((server) => server.name === "m3" ? { ...server, online: false } : server);
    let steps = runSteps(["db", "db-2", "db-3", "db-etcd", "db-router"], ["db"], "db", placement);
    for (let round = 0; round < 10; round += 1) steps = advance(steps, servers);
    expect(states(steps)).toEqual([
      ["Repoint db", "done"],
      ["Redeploy consumers", "done"],
      ["Remove db-etcd, db-router", "done"],
      ["Remove db-2", "done"],
      ["Remove db-3", { waiting: "m3" }],
    ]);
    expect(states(removeServer(steps, "m3")).at(-1)).toEqual(["Remove db-3", "done"]);
  });
});
