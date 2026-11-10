import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import {
  CLUSTER_REQUIREMENTS,
  type ClusterFeature,
  featuresOf,
  foundingFeatures,
  joinWords,
  missingFor,
  serversMissing,
  skipWords,
} from "#/modules/cluster-requirements/cluster-requirements";

const CLOSED_DOORS = "ployz.ingress.closed-doors.v1";
const closed = new Set<ClusterFeature>(["closedDoors"]);
const none = new Set<ClusterFeature>();

type Frame = NonNullable<Parameters<typeof featuresOf>[0]["frame"]>;

const holder = (ports: readonly number[]) => ({
  identity: "ployz-system/ingress-holder",
  containers: [{ machine_id: "m1", runtime: { state: "running" }, resolved_spec: { ports: ports.map((port) => ({ mode: "host", published_port: port })) } }],
});

const frame = (services: readonly ReturnType<typeof holder>[], partial = false): Frame => asTestDouble<Frame>()({
  services,
  machines: [],
  incomplete_ids: { machines: partial ? ["m2"] : [], containers: [] },
  unanswered: [],
});

describe("missingFor", () => {
  it("names the feature when an old CLI lists nothing and the Cluster requires closed doors", () => {
    expect(missingFor(closed, undefined)).toBe("closedDoors");
    expect(missingFor(closed, [])).toBe("closedDoors");
    expect(missingFor(closed, ["ployz.logs.tail.v1"])).toBe("closedDoors");
  });

  it("admits a CLI that lists the capability", () => {
    expect(missingFor(closed, [CLOSED_DOORS])).toBeNull();
  });

  it("requires nothing of a default Cluster, so a CLI that lists nothing is admitted as today", () => {
    expect(missingFor(none, undefined)).toBeNull();
    expect(missingFor(none, [])).toBeNull();
  });

  it("names the capability the table maps the feature to", () => {
    expect(CLUSTER_REQUIREMENTS.closedDoors.requirement.capability).toBe(CLOSED_DOORS);
  });
});

describe("foundingFeatures", () => {
  it("reads a closed founding from the Fleet Token", () => {
    expect(foundingFeatures({ foundsClosed: true })).toEqual(closed);
    expect(foundingFeatures({ foundsClosed: false })).toEqual(none);
    expect(foundingFeatures({})).toEqual(none);
  });
});

describe("featuresOf", () => {
  it("reads a closed Cluster from a holder that publishes no port, with nothing recorded", () => {
    expect(featuresOf({ recorded: none, frame: frame([holder([])]) })).toEqual(closed);
  });

  it("lets a complete frame whose holders publish 80 and 443 win over a stale record", () => {
    expect(featuresOf({ recorded: closed, frame: frame([holder([80, 443])]) })).toEqual(none);
  });

  it("keeps the record when a complete frame shows no holder yet", () => {
    expect(featuresOf({ recorded: closed, frame: frame([]) })).toEqual(closed);
    expect(featuresOf({ recorded: none, frame: frame([]) })).toEqual(none);
  });

  it("adds the record to a partial frame, since a Server not seen may hold the closed holder", () => {
    expect(featuresOf({ recorded: closed, frame: frame([holder([80, 443])], true) })).toEqual(closed);
    expect(featuresOf({ recorded: none, frame: frame([holder([80, 443])], true) })).toEqual(none);
  });

  it("falls back to the record when there is no frame", () => {
    expect(featuresOf({ recorded: closed, frame: null })).toEqual(closed);
    expect(featuresOf({ recorded: none, frame: null })).toEqual(none);
  });
});

describe("serversMissing", () => {
  const machine = (name: string, capabilities: readonly string[], acceptsIngress = true) => ({
    machine: { name, accepts_ingress: acceptsIngress, runtime: { capabilities } },
  });

  it("names a Server whose list lacks the capability, and one that lists none", () => {
    const seen = asTestDouble<Parameters<typeof serversMissing>[0]>()({
      machines: [machine("alpha", [CLOSED_DOORS]), machine("beta", ["ployz.logs.tail.v1"]), machine("gamma", [])],
    });
    expect(serversMissing(seen, closed)).toEqual(["beta", "gamma"]);
  });

  it("looks only at the Servers the filter keeps", () => {
    const seen = asTestDouble<Parameters<typeof serversMissing>[0]>()({
      machines: [machine("web", [], true), machine("worker", [], false)],
    });
    expect(serversMissing(seen, closed, (each) => each.accepts_ingress)).toEqual(["web"]);
  });
});

describe("words", () => {
  it("tells a joiner what the Cluster keeps closed and to update ployz", () => {
    expect(joinWords("closedDoors")).toBe("This Cluster keeps ports 80 and 443 closed. Update ployz on this server to join it.");
  });

  it("gives a skip reason that names the feature", () => {
    expect(skipWords("closedDoors")).toBe("Update ployz to join a Cluster with closed ports 80 and 443");
  });
});
