import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import { doorsGap, movedHostnames, switchProblem, switchProgress, type CloudflareSetup } from "#/modules/ingress/doors";
import { runSwitch, switchSteps, type SwitchPorts } from "#/modules/ingress/doors.run";

const answer = (...perServer: ReadonlyArray<readonly string[]>) => ({
  machines: perServer.map((doors, index) => ({ server: `s${index + 1}`, doors })),
  failures: [],
});

/** Ports that record what the switch did, with the Doors each Server holds after every set. */
function ports(held: (doors: readonly string[]) => ReturnType<typeof answer>, log: string[]): SwitchPorts<never, never, never> {
  return {
    open: (doors) => Effect.sync(() => { log.push(`open ${doors.join("+")}`); return held(doors); }),
    at: (stage) => Effect.sync(() => { log.push(`at ${stage}`); }),
    flip: () => Effect.sync(() => { log.push("flip"); }),
    redeploy: () => Effect.sync(() => { log.push("redeploy"); }),
  };
}

describe("the two-step door switch", () => {
  it("opens both Doors, moves the names, then closes the old Door", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(runSwitch("tunnel", ports((doors) => answer(doors, doors), log)));
    expect(end).toEqual({ ok: true });
    expect(log).toEqual(["at opening", "open public+tunnel", "at moving", "flip", "redeploy", "at closing", "open tunnel"]);
  });

  it("goes back the same way, ending on public ports", async () => {
    const log: string[] = [];
    await Effect.runPromise(runSwitch("public", ports((doors) => answer(doors), log)));
    expect(log.filter((line) => line.startsWith("open"))).toEqual(["open public+tunnel", "open public"]);
    expect(switchSteps("public")).toEqual([["public", "tunnel"], ["public"]]);
  });

  it("stops before moving anything when a Server doesn't take both Doors", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(runSwitch("tunnel", ports(() => answer(["public", "tunnel"], ["public"]), log)));
    expect(end).toEqual({ ok: false, failure: "s2 still has other doors open." });
    expect(log).toEqual(["at opening", "open public+tunnel"]);
  });

  it("leaves both Doors open when closing the old one fails", async () => {
    const log: string[] = [];
    const end = await Effect.runPromise(runSwitch("tunnel", ports((doors) => doors.length === 2 ? answer(doors) : answer(["public", "tunnel"]), log)));
    expect(end).toMatchObject({ ok: false });
    expect(log.at(-1)).toBe("open tunnel");
  });

  it("reports a Server that refused, by its own words", () => {
    expect(doorsGap({ machines: [], failures: [{ error: { message: "no connector" } }] }, ["tunnel"])).toBe("A server didn't switch: no connector");
    expect(doorsGap(answer(["tunnel"]), ["tunnel"])).toBeNull();
    expect(doorsGap(answer(), ["tunnel"])).toBe("No server takes web traffic yet.");
  });
});

describe("what a switch moves", () => {
  const published = [
    { hostname: "web-prod.acme.ployz.app", namespace: "shop-prod", service: "web" },
    { hostname: "deep.web-prod.acme.ployz.app", namespace: "shop-prod", service: "deep" },
    { hostname: "www.example.org", namespace: "shop-prod", service: "web" },
  ];

  it("moves each generated hostname one label below the new source, and leaves custom domains alone", () => {
    expect(movedHostnames(published, "acme.ployz.app", "example.com")).toEqual([
      { from: "web-prod.acme.ployz.app", to: "web-prod.example.com", namespace: "shop-prod", service: "web" },
    ]);
  });
});

describe("when the Cluster can switch", () => {
  const setup = (state: CloudflareSetup["state"]): CloudflareSetup => ({ zone: "example.com", tunnelName: "ployz-acme", tunnelId: "t1", state });

  it("goes to Cloudflare only once it is set up and not yet in use", () => {
    expect(switchProblem(null, "tunnel")).toBe("Set up Cloudflare first, then switch to it.");
    expect(switchProblem(setup("set_up"), "tunnel")).toBeNull();
    expect(switchProblem(setup("active"), "tunnel")).toBe("This Cluster already uses Cloudflare.");
    expect(switchProblem(setup("left"), "tunnel")).toBe("Set up Cloudflare again, then switch to it.");
  });

  it("goes back to public ports from anywhere but public ports", () => {
    expect(switchProblem(setup("active"), "public")).toBeNull();
    expect(switchProblem(setup("left"), "public")).toBe("This Cluster already uses public ports.");
  });
});

describe("progress", () => {
  it("says where a switch is, and nothing once it ended", () => {
    const base = { attemptId: "a", want: "tunnel" as const };
    expect(switchProgress(null)).toBeNull();
    expect(switchProgress({ ...base, state: "finished", endedAt: "2026-11-08T00:00:00Z" })).toBeNull();
    expect(switchProgress({ ...base, state: "running", startedAt: "2026-11-08T00:00:00Z", stage: "moving" })).toMatchObject({ step: 2 });
    expect(switchProgress({ ...base, state: "running", startedAt: "2026-11-08T00:00:00Z", stage: null })).toMatchObject({ step: 1 });
  });
});
