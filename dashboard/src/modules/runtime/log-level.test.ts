import { expect, it } from "vitest";
import { type LogLevel, logLevel } from "./log-level";

it.each<[string, LogLevel]>([
  [`{"level":"error","msg":"x"}`, "error"],
  [`{"lvl":"WARN"}`, "warn"],
  [`{"severity":"debug"}`, "debug"],
  [`{"Level":"warning"}`, "warn"],
  ["ts=1 level=error msg=boom", "error"],
  [`msg="x" level="debug"`, "debug"],
  ["ERROR something broke", "error"],
  ["warn: disk at 90%", "warn"],
  ["Debug starting", "debug"],
  ["[error] tagged", "error"],
  ["[WARN] tagged", "warn"],
  ["  INFO indented", "info"],
  ["2026/10/18 14:42:00 [error] 29#29: *1 open() failed", "error"],
  ["2026-10-18T14:42:00.123Z WARN slow query", "warn"],
  [`{"level":"debug","severity":"error"}`, "debug"],
  [`{"level":"warn","msg":"level=error"}`, "warn"],
  ["ERROR level=debug", "debug"],
  ["[error] level=warn", "warn"],
])("reads %s as %s", (line, level) => {
  expect(logLevel(line)).toBe(level);
});

it.each([
  "",
  "plain text on stdout",
  "Errors are counted later",
  "errored out",
  "[req-12] handled",
  "500 error page served",
  "2026/10/18 14:42:00 served /",
  `{"level":7}`,
  `{"level":"loud"}`,
  "{not json level=",
  "level=loud",
  "�� binary",
])("reads %j as info", (line) => {
  expect(logLevel(line)).toBe("info");
});
