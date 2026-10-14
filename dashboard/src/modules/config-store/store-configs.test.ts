import { Result, Schema } from "effect";
import { describe, expect, it } from "vitest";
import type { ConfigItemView, ConfigListing, DiffView, EnvironmentView, RowId, ServiceListing } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import {
  attachConfigCommand, configEdits, configFileSizeError, configReferenceValues, configTrays, createConfigCommand, draftAccess, executableToggle,
  fileAccessDraftSchema, fileAccessLabel, putConfigFileCommand, saveConfigCommand, utf8Bytes,
} from "./store-configs";

const environment = { project: "shop", environment: "production" };

describe("Config commands", () => {
  it("creates an empty Config mounted nowhere, with the minted id", () => {
    expect(createConfigCommand("c1", environment, "sentry"))
      .toEqual({ command: "create_config", id: "c1", environment, name: "sentry", mounts: [] });
  });

  it("puts a file's text and sends only the access fields the user set, so the Store keeps the others", () => {
    expect(putConfigFileCommand(environment, "sentry", "config.yml", "a: 1"))
      .toEqual({ command: "put_config_file", environment, config: "sentry", file: "config.yml", content: "a: 1" });
    expect(putConfigFileCommand(environment, "sentry", "run.sh", "#!/bin/sh", { mode: "0555" }))
      .toEqual({ command: "put_config_file", environment, config: "sentry", file: "run.sh", content: "#!/bin/sh", mode: "0555" });
  });

  it("saves every edited file of a Config in one batch", () => {
    const save = saveConfigCommand(environment, "sentry", [{ file: "config.yml", content: "a" }, { file: "run.sh", content: "b", mode: "0555" }]);
    expect(save).toEqual({ command: "batch", environment, commands: [
      { command: "put_config_file", environment, config: "sentry", file: "config.yml", content: "a" },
      { command: "put_config_file", environment, config: "sentry", file: "run.sh", content: "b", mode: "0555" },
    ] });
  });

  it("carries a file's owner in the batch, including root's 0", () => {
    const save = saveConfigCommand(environment, "sentry", [{ file: "app.conf", content: "a", mode: "0640", uid: 0, gid: 1000 }]);
    expect(save).toEqual({ command: "batch", environment, commands: [
      { command: "put_config_file", environment, config: "sentry", file: "app.conf", content: "a", mode: "0640", uid: 0, gid: 1000 },
    ] });
  });

  it("mounts a Config on a Service at a directory", () => {
    expect(attachConfigCommand(environment, "web", "sentry", "/etc/sentry"))
      .toEqual({ command: "attach_config", environment, service: "web", config: "sentry", dir: "/etc/sentry" });
  });
});

describe("Config files", () => {
  it("toggles only root-owned 0444 and 0555; anything else is custom and labels its owner", () => {
    expect(executableToggle({ mode: "0444", uid: 0, gid: 0 })).toEqual({ executable: false });
    expect(executableToggle({ mode: "0555", uid: 0, gid: 0 })).toEqual({ executable: true });
    expect(executableToggle({ mode: "0640", uid: 0, gid: 0 })).toBeNull();
    expect(executableToggle({ mode: "0444", uid: 999, gid: 999 })).toBeNull();
    expect(fileAccessLabel({ mode: "0555", uid: 0, gid: 0 })).toBe("0555");
    expect(fileAccessLabel({ mode: "0640", uid: 0, gid: 1000 })).toBe("0640 · 0:1000");
  });

  it("shows a draft's access over the stored file's, and a new file's defaults", () => {
    expect(draftAccess(undefined, undefined)).toEqual({ mode: "0444", uid: 0, gid: 0 });
    expect(draftAccess({ mode: "0640", uid: 1000, gid: 1000 }, { content: "", gid: 0 })).toEqual({ mode: "0640", uid: 1000, gid: 0 });
  });

  it("refuses text over 256 KB, counted in UTF-8 bytes", () => {
    expect(configFileSizeError(256 * 1024)).toBeNull();
    expect(configFileSizeError(300 * 1024)).toBe("Over the 256 KB limit (300 KB).");
    expect(utf8Bytes("é")).toBe(2);
  });
});

describe("Config trays", () => {
  const listing = (id: string, name: string): ServiceListing => ({ id, row: `${id}:node` as RowId, name, private_dns: name, source: "image", change: null, template: null });
  const config = (id: string, mounts: { service: string; dir: string }[]): ConfigListing =>
    ({ id, name: id, files: [], mounts, deployed: true, change: null });
  const services = [listing("s1", "web"), listing("s2", "worker")];
  const configs = [
    config("sentry", [{ service: "web", dir: "/etc/sentry" }, { service: "worker", dir: "/etc/sentry" }]),
    { ...config("relay", [{ service: "worker", dir: "/etc/relay" }]), id: "5802594f-5019-45d9-abbd-7257780c8565" },
    config("loose", []),
    config("orphan", [{ service: "gone", dir: "/etc" }]),
  ];
  const diff = asTestDouble<DiffView>()({ changes: [{ type: "service", id: "s2", name: "worker", lifecycle: "update", comparison: "head", data: null,
    settings: [{ path: "worker.configs.@5802594f-5019-45d9-abbd-7257780c8565", kind: "add", before: null, after: "/etc/relay", canRestore: true }] }] });
  const { trays, unmounted } = configTrays(services, configs, diff);

  it("puts a mounted Config in a tray under each Service that mounts it, with its directory", () => {
    expect(trays.get("s1")?.map((tray) => [tray.config.id, tray.dir])).toEqual([["sentry", "/etc/sentry"]]);
    expect(trays.get("s2")?.map((tray) => [tray.config.id, tray.dir])).toEqual([["sentry", "/etc/sentry"], ["5802594f-5019-45d9-abbd-7257780c8565", "/etc/relay"]]);
  });

  it("marks only the mount the next Deploy stages", () => {
    expect(trays.get("s2")?.map((tray) => [tray.config.id, tray.mountChanged])).toEqual([["sentry", false], ["5802594f-5019-45d9-abbd-7257780c8565", true]]);
  });

  it("leaves a Config no Service here mounts as its own node", () => {
    expect(unmounted.map((listing) => listing.id)).toEqual(["loose", "orphan"]);
  });
});

describe("Saving drafts", () => {
  const stored = asTestDouble<ConfigItemView>()({
    files: [{ name: "config.yml", bytes: 4, mode: "0444", uid: 0, gid: 0, references: [] }],
    contents: { "config.yml": "a: 1" },
  });

  it("writes only drafts that differ from the Store, and new files", () => {
    const drafts = new Map([["config.yml", { content: "a: 1" }], ["new.toml", { content: "" }]]);
    expect(configEdits(stored, drafts)).toEqual([{ file: "new.toml", content: "" }]);
  });

  it("writes a file whose text is unchanged when only its owner changed", () => {
    expect(configEdits(stored, new Map([["config.yml", { content: "a: 1", mode: "0444", uid: 1000, gid: 0 }]])))
      .toEqual([{ file: "config.yml", content: "a: 1", mode: "0444", uid: 1000, gid: 0 }]);
    expect(configEdits(stored, new Map([["config.yml", { content: "a: 1", mode: "0444", uid: 0, gid: 0 }]]))).toEqual([]);
  });

  it("writes a file whose text is unchanged when only its mode flipped", () => {
    expect(configEdits(stored, new Map([["config.yml", { content: "a: 1", mode: "0555" }]])))
      .toEqual([{ file: "config.yml", content: "a: 1", mode: "0555" }]);
    expect(configEdits(stored, new Map([["config.yml", { content: "a: 1", mode: "0444" }]]))).toEqual([]);
  });

  it("never empties a hidden file: a mode change alone writes nothing, new text replaces it", () => {
    const hidden = { ...stored, contents: {} };
    expect(configEdits(hidden, new Map([["config.yml", { content: "", mode: "0555" }]]))).toEqual([]);
    expect(configEdits(hidden, new Map([["config.yml", { content: "b: 2", mode: "0555" }]])))
      .toEqual([{ file: "config.yml", content: "b: 2", mode: "0555" }]);
  });

  it("retries a failed save even when the cache holds its text, but never empties a hidden file on retry", () => {
    const failed = new Set(["config.yml"]);
    expect(configEdits(stored, new Map([["config.yml", { content: "a: 1" }]]), failed)).toEqual([{ file: "config.yml", content: "a: 1" }]);
    const hidden = { ...stored, contents: {} };
    expect(configEdits(hidden, new Map([["config.yml", { content: "" }]]), failed)).toEqual([]);
  });

  it("still writes a readable file left empty on purpose", () => {
    expect(configEdits(stored, new Map([["config.yml", { content: "" }]]))).toEqual([{ file: "config.yml", content: "" }]);
  });
});

describe("Preview values", () => {
  const view = asTestDouble<EnvironmentView>()({
    environment: { id: "env-1", name: "production" },
    settings: [
      { path: "redis.env.PORT", value: "6379" },
      { path: "api.env.SECRET_KEY", value: { secret: true } },
    ],
  });
  const services = [
    { id: "s1", row: "s1:node" as RowId, name: "redis", private_dns: "redis", source: "image", change: null, template: null },
    { id: "s2", row: "s2:node" as RowId, name: "api", private_dns: "api", source: "image", change: null, template: null },
  ] satisfies ServiceListing[];
  const values = configReferenceValues(view, services);

  it("resolves a Service's private address and its own PORT over the default", () => {
    expect(values.get("redis.PLOYZ_PRIVATE_DOMAIN")).toEqual({ secret: false, value: "redis.internal" });
    expect(values.get("redis.PORT")).toEqual({ secret: false, value: "6379" });
    expect(values.get("api.PORT")).toEqual({ secret: false, value: "8080" });
  });

  it("carries no value for a secret", () => {
    expect(values.get("api.SECRET_KEY")).toEqual({ secret: true });
  });
});

it("keeps detached mounts separate from a replacement Config with the same name", async () => {
  const { detachedConfigMounts } = await import("./store-configs");
  const { asTestDouble } = await import("#/lib/test-double");
  const diff = asTestDouble<import("@ployz/sdk").DiffView>()({ changes: [{ type: "service", id: "web-id", name: "web", settings: [
    { path: "web.configs.@old-id", configName: "app", before: "/etc/old", after: null },
    { path: "web.configs.@new-id", configName: "app", before: null, after: "/etc/new" },
  ] }] });
  expect(detachedConfigMounts(diff, "old-id")).toEqual([{ serviceId: "web-id", service: "web", directory: "/etc/old" }]);
  expect(detachedConfigMounts(diff, "new-id")).toEqual([]);
});

describe("Permissions dialog", () => {
  const parse = (draft: { mode: string; uid: string; gid: string }) => Schema.decodeUnknownResult(fileAccessDraftSchema)(draft);
  const failure = (draft: { mode: string; uid: string; gid: string }) => {
    const parsed = parse(draft);
    return Result.isFailure(parsed) ? parsed.failure.message : null;
  };

  it("parses three octal digits, with an optional leading 0, into the Store's four-digit mode", () => {
    expect(parse({ mode: "640", uid: "1000", gid: " 1000 " })).toEqual(Result.succeed({ mode: "0640", uid: 1000, gid: 1000 }));
    expect(parse({ mode: "0755", uid: "0", gid: "0" })).toEqual(Result.succeed({ mode: "0755", uid: 0, gid: 0 }));
  });

  it("refuses a mode that is not three octal digits", () => {
    for (const mode of ["999", "0888", "64", "10644", "", "rwx"]) {
      expect(failure({ mode, uid: "0", gid: "0" })).toContain("Mode is three octal digits, like 0644.");
    }
  });

  it("refuses an owner that is not a whole number the Store can hold", () => {
    expect(failure({ mode: "0644", uid: "-1", gid: "0" })).toContain("User ID is a whole number, like 1000.");
    expect(failure({ mode: "0644", uid: "0", gid: "1.5" })).toContain("Group ID is a whole number, like 1000.");
    expect(failure({ mode: "0644", uid: "4294967296", gid: "0" })).toContain("User ID is a whole number, like 1000.");
    expect(parse({ mode: "0644", uid: "4294967295", gid: "0" })).toEqual(Result.succeed({ mode: "0644", uid: 4294967295, gid: 0 }));
  });

  it("prefills the fields from a file's access", () => {
    expect(Schema.encodeSync(fileAccessDraftSchema)({ mode: "0640", uid: 0, gid: 1000 })).toEqual({ mode: "0640", uid: "0", gid: "1000" });
  });
});
