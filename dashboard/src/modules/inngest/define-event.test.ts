import { Option, Schema } from "effect";
import { describe, expect, it } from "vitest";
import { defineEvent, eventData } from "#/modules/inngest/define-event";

const Probe = Schema.Struct({ attemptId: Schema.String.check(Schema.isUUID()) });
const keyed = defineEvent({ name: "probe/requested", schema: Probe, id: (data) => `probe-${data.attemptId}` });
const unkeyed = defineEvent({ name: "probe/noted", schema: Probe });
const attemptId = "6f1c1f5e-6b0e-4a43-8f43-1e0a8f0b9a11";

describe("defineEvent", () => {
  it("keys an event by its id and leaves an unkeyed one without", () => {
    expect(keyed.create({ attemptId })).toEqual({ id: `probe-${attemptId}`, name: "probe/requested", data: { attemptId } });
    expect(unkeyed.create({ attemptId })).toEqual({ name: "probe/noted", data: { attemptId } });
  });

  it("decodes an event's data, keeping a property the schema doesn't name, and answers none for an invalid one", () => {
    expect(eventData(keyed)({ data: { attemptId, extra: 1 } })).toEqual(Option.some({ attemptId, extra: 1 }));
    expect(Option.isNone(eventData(keyed)({ data: { attemptId: "nope" } }))).toBe(true);
  });
});
