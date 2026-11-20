import { expect, it } from "vitest";
import { crashEventsOf, MAX_CRASH_RECORDS } from "#/modules/observe-ingest/crash-logs";

const record = (index: number, code: string | number = "137") => ({
  timeUnixNano: "1760000000000000000",
  attributes: [
    { key: "event.name", value: { stringValue: "ployz.container.exit" } },
    { key: "ployz.event.id", value: { stringValue: `${"b".repeat(64)}:${index}` } },
    { key: "ployz.namespace", value: { stringValue: "prod" } },
    { key: "ployz.service.name", value: { stringValue: "web" } },
    { key: "ployz.service.id", value: { stringValue: "c".repeat(32) } },
    { key: "container.id", value: { stringValue: "b".repeat(64) } },
    { key: "ployz.replica", value: { stringValue: "1" } },
    { key: "ployz.exit.code", value: { intValue: code } },
    { key: "ployz.exit.oom", value: { boolValue: true } },
  ],
});

const body = (records: ReadonlyArray<unknown>) => JSON.stringify({ resourceLogs: [{ scopeLogs: [{ logRecords: records }] }] });

it("reads an exit code sent as a decimal string or a number, and the time as unix nanos", () => {
  const [fromString, fromNumber] = crashEventsOf(body([record(1, "137"), record(2, 1)]));
  expect(fromString).toMatchObject({ exitCode: 137, oom: true, serviceName: "prod/web", occurredAt: new Date(1_760_000_000_000) });
  expect(fromNumber?.exitCode).toBe(1);
});

it("handles at most 1000 records of a body", () => {
  const records = Array.from({ length: MAX_CRASH_RECORDS + 5 }, (_, index) => record(index));
  expect(crashEventsOf(body(records))).toHaveLength(MAX_CRASH_RECORDS);
});

it("finds nothing in a body that is not OTLP logs", () => {
  for (const json of ["null", "x", "[]", '{"resourceLogs":"no"}', '{"resourceLogs":[{"scopeLogs":[{"logRecords":"no"}]}]}']) {
    expect(crashEventsOf(json)).toEqual([]);
  }
});
