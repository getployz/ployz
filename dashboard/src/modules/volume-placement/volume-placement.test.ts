import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { Member } from "#/modules/volume-run/volume-run";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
import {
  detachedCopies,
  failover,
  failoverWords,
  observePlacement,
  placementChanges,
  placementSteps,
  placementTargets,
  placementWords,
  type PlacementRow,
} from "./volume-placement";

const volume = { id: "v1", name: "pg-data" };
const id = (server: string) => server.padEnd(32, "0") as MachineId;
const row = (mirrors: string[] | null, staged: { primary: string; mirrors?: string[]; deletes?: string[] } | null = null): PlacementRow => ({
  volumeId: "v1",
  environmentId: "e1",
  mirrors: mirrors === null ? null : mirrors.map(id),
  staged: staged === null ? null : { primary: id(staged.primary), mirrors: (staged.mirrors ?? []).map((name) => id(name)), deletes: (staged.deletes ?? []).map((name) => id(name)) },
  deploymentId: null,
});
const pooled = (server: string) => member(server, "empty", { pool: true });
const at = new Date(2026, 9, 10, 10, 36);
const mirrorAsOf = (server: string, when: Date): Member => {
  const base = member(server, "mirror");
  if (!base.answered || base.view.copy === null) throw new Error("fixture");
  return { ...base, view: { ...base.view, copy: { ...COPIES.mirror, newest: { name: "s", guid: "12", created_unix_seconds: when.getTime() / 1000 } } } };
};
const changes = (members: Member[], placement: PlacementRow | null) => placementChanges(volume, members, placement);
const words = (members: Member[], placement: PlacementRow | null) => changes(members, placement).map(placementWords);
const steps = (members: Member[], placement: PlacementRow | null) => placementSteps(changes(members, placement));

describe("placementChanges", () => {
  it("is empty when nothing is staged", () => {
    expect(changes([member("web-1", "writer"), member("web-2", "mirror")], row(["web-2"]))).toEqual([]);
  });

  it("makes a move one change, even when it consumes the mirror it promotes", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")];
    const staged = changes(members, row(["web-2"], { primary: "web-2" }));
    expect(staged.map((change) => change.kind)).toEqual(["move"]);
    expect(words(members, row(["web-2"], { primary: "web-2" }))).toEqual([
      { title: "Move pg-data to web-2", detail: "Uses the mirror on web-2, then a short stop. web-1 keeps pg-data-old." },
    ]);
  });

  it("copies first when the target holds nothing, and leaves the mirror list alone", () => {
    const members = [member("web-1", "writer"), pooled("web-3")];
    expect(words(members, row([], { primary: "web-3" }))).toEqual([
      { title: "Move pg-data to web-3", detail: "Copies pg-data to web-3 first, then a short stop." },
    ]);
    expect(steps(members, row([], { primary: "web-3" }))).toEqual([{ kind: "move", args: { to: "web-3" } }]);
  });

  it("restores from the mirror when the primary does not answer, and says what is lost", () => {
    const members = [member("web-1", "unanswered"), mirrorAsOf("web-2", at)];
    const [change] = changes(members, row(["web-2"], { primary: "web-2" }));
    expect(change).toMatchObject({ kind: "move", how: "restore", lostAfter: at, from: { name: "web-1" } });
    expect(words(members, row(["web-2"], { primary: "web-2" }))).toEqual([
      { title: "Use mirror on web-2", detail: "web-1 is not answering. Changes after 10:36 lost." },
    ]);
    expect(steps(members, row(["web-2"], { primary: "web-2" }))).toEqual([{ kind: "restore", args: { from: "web-2" } }]);
  });

  it("treats a copy not listed in mirrors as detached", () => {
    const members = [member("web-2", "writer"), member("web-1", "mirror")];
    expect(detachedCopies(observePlacement(members), row([])).map((copy) => copy.server.name)).toEqual(["web-1"]);
    expect(detachedCopies(observePlacement(members), row(["web-1"]))).toEqual([]);
  });

  it("counts every copy as a mirror before the dashboard has a record", () => {
    expect(detachedCopies(observePlacement([member("web-1", "writer"), member("web-2", "mirror")]), null)).toEqual([]);
  });

  it("deletes a detached copy before adding a mirror, and the review says so", () => {
    const members = [member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")];
    const placement = row([], { primary: "web-2", mirrors: ["web-3"] });
    expect(words(members, placement)).toEqual([{
      title: "Add mirror on web-3",
      detail: "Copies pg-data to web-3. Refreshes on every deploy. Deletes pg-data-old on web-1 first.",
    }]);
    expect(steps(members, placement)).toEqual([
      { kind: "delete_mirror", args: { slot: "web-1", confirmed_name: null } },
      { kind: "mirror", args: { to: "web-3" } },
    ]);
  });

  it("adopts a detached copy as the mirror on its own Server without deleting it", () => {
    const members = [member("web-2", "writer"), member("web-1", "mirror")];
    expect(steps(members, row([], { primary: "web-2", mirrors: ["web-1"] }))).toEqual([{ kind: "mirror", args: { to: "web-1" } }]);
  });

  it("deletes a staged detached copy and nothing else", () => {
    const members = [member("web-2", "writer"), member("web-1", "mirror")];
    expect(words(members, row([], { primary: "web-2", deletes: ["web-1"] }))).toEqual([
      { title: "Delete pg-data-old", detail: "Deletes the old copy on web-1." },
    ]);
  });

  it("removes a mirror the user took off the list", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror")];
    expect(words(members, row(["web-2"], { primary: "web-1" }))).toEqual([{ title: "Remove mirror on web-2", detail: "Deletes pg-data-web-2." }]);
  });

  it("orders runs so deletions free the copy slot before a move, and a new mirror comes last", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")];
    expect(steps(members, row(["web-2"], { primary: "web-2", mirrors: ["web-3"] }))).toEqual([
      { kind: "move", args: { to: "web-2" } },
      { kind: "mirror", args: { to: "web-3" } },
    ]);
    expect(steps(members, row(["web-2"], { primary: "web-3" }))).toEqual([
      { kind: "delete_mirror", args: { slot: "web-2", confirmed_name: null } },
      { kind: "move", args: { to: "web-3" } },
    ]);
  });
});

describe("failover", () => {
  it("offers the mirror when the primary stops answering, with the loss time from its newest snapshot", () => {
    const members = [member("web-1", "unanswered"), mirrorAsOf("web-2", at)];
    const offer = failover(observePlacement(members), row(["web-2"]));
    expect(offer && failoverWords(offer)).toBe("web-1 is not answering · Use mirror on web-2 · Changes after 10:36 lost");
  });

  it("offers nothing once the mirror is staged, or while the primary answers", () => {
    const members = [member("web-1", "unanswered"), mirrorAsOf("web-2", at)];
    expect(failover(observePlacement(members), row(["web-2"], { primary: "web-2" }))).toBeNull();
    expect(failover(observePlacement([member("web-1", "writer"), member("web-2", "unanswered")]), row([]))).toBeNull();
  });

  it("does not offer a detached copy", () => {
    expect(failover(observePlacement([member("web-1", "unanswered"), member("web-2", "mirror")]), row([]))).toBeNull();
  });
});

describe("placementTargets", () => {
  const servers = ["web-1", "web-2", "web-3"].map(id);

  it("offers a move only to the mirror's Server while a mirror exists", () => {
    const members = [member("web-1", "writer"), member("web-2", "mirror"), pooled("web-3")];
    const targets = placementTargets(volume, members, row(["web-2"]), servers);
    expect(targets.move.map((server) => server.name)).toEqual(["web-2"]);
    expect(targets.mirror).toEqual([]);
  });

  it("offers every other Server once the only other copy is detached, since the Deploy deletes it first", () => {
    const members = [member("web-2", "writer"), member("web-1", "mirror"), pooled("web-3")];
    const targets = placementTargets(volume, members, row([]), servers);
    expect(targets.move.map((server) => server.name)).toEqual(["web-1", "web-3"]);
    expect(targets.mirror.map((server) => server.name)).toEqual(["web-1", "web-3"]);
  });

  it("offers nothing on a Server that does not accept Services", () => {
    const members = [member("web-1", "writer"), pooled("web-2"), pooled("web-3")];
    expect(placementTargets(volume, members, row([]), [id("web-2")]).move.map((server) => server.name)).toEqual(["web-2"]);
  });
});
