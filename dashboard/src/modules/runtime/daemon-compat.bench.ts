import { bench, describe } from "vitest";
import { lacks, requirementFor, requirements } from "#/modules/runtime/daemon-compat";
import { remedy, serverUpgradeLine, type ReleaseState, type ServerUpgradeLine } from "#/modules/server-upgrade/server-upgrade";

const caps = [requirements.liveLogs.capability, requirements.olderLogs.capability, undefined];
const failures = Array.from({ length: 50 }, (_, i) => ({
  machineId: `m-${i}`,
  message: `failure ${i}`,
  code: i % 3 === 2 ? undefined : "unsupported",
  capability: caps[i % 3],
}));

const releases: ReleaseState[] = [
  { state: "resolved", release: "0.2.4" },
  { state: "resolved", release: "0.2.3" },
  { state: "pending" },
  { state: "failed" },
];
const lines: ServerUpgradeLine[] = [
  { kind: "behind", release: "0.2.4", canUpgrade: true },
  { kind: "upgrading", target: "0.2.4" },
  { kind: "when-back", release: "0.2.4" },
  null,
  { kind: "failed", target: "0.2.4", nothingChanged: true, details: null, canRetry: true },
];
const servers = Array.from({ length: 50 }, (_, i) => ({
  feature: i % 7 === 6 ? null : requirementFor(i % 2 ? requirements.olderLogs.capability : requirements.liveLogs.capability),
  server: { name: `web-${i}`, daemonVersion: i % 5 === 4 ? "" : "0.2.3" },
  release: releases[i % 4] ?? { state: "pending" },
  line: lines[i % 5] ?? null,
}));

let sink = 0;
describe("diff-added work per page", () => {
  bench("lacks over 50 failures", () => {
    const out = lacks(failures);
    sink += out.lacks.length + out.rest.length;
    if (out.lacks.length !== 34 || out.rest.length !== 16) throw new Error(`bad split ${out.lacks.length}/${out.rest.length}`);
  });
  bench("remedy over 50 Servers (one bench call = 50 remedy calls)", () => {
    for (const s of servers) sink += remedy(s).kind.length;
  });
  bench("serverUpgradeLine + remedy over 50 Servers", () => {
    for (const s of servers) {
      const line = serverUpgradeLine({ version: "0.2.3", status: "online", release: "0.2.4", latest: null, pendingFrom: undefined, now: 0, automatic: false, rolloutRunning: false });
      sink += remedy({ ...s, line }).kind.length;
    }
  });
});
export { sink };
