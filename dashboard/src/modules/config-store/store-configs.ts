import { Schema, SchemaGetter } from "effect";
import type {
  AttachConfig, ConfigCommand, ConfigFileSummary, ConfigItemView, ConfigListing, CreateConfig, DiffView, EnvironmentRef, EnvironmentView,
  PutConfigFile, ServiceListing,
} from "@ployz/sdk";
import type { ReferenceValue } from "./config-references";
import { serviceChanges, serviceSettingRows, settingText } from "./store-services";
import { serviceVariables, storeManagedExports } from "./store-variables";

/** The most text one Config file holds, in bytes; the Store's `MAX_FILE_BYTES`. */
export const CONFIG_FILE_MAX_BYTES = 256 * 1024;

/** The mode a Config file gets unless the user marks it executable. */
export const READ_ONLY_MODE = "0444";
export const EXECUTABLE_MODE = "0555";

/** The command that creates an empty Config with the id the caller minted, mounted nowhere yet. */
export function createConfigCommand(id: string, environment: EnvironmentRef, name: string): { command: "create_config" } & CreateConfig {
  return { command: "create_config", id, environment, name, mounts: [] };
}

/** A file's permission bits and owner, as the Store keeps them. */
export type FileAccess = Pick<ConfigFileSummary, "mode" | "uid" | "gid">;

/** What a new file gets: read-only, owned by root. */
export const DEFAULT_ACCESS: FileAccess = { mode: READ_ONLY_MODE, uid: 0, gid: 0 };

/** Writes one file's text, and only the access fields the user changed, so the Store keeps the rest. */
export function putConfigFileCommand(environment: EnvironmentRef, config: string, file: string, content: string, access: Partial<FileAccess> = {}):
  { command: "put_config_file" } & PutConfigFile {
  return { command: "put_config_file", environment, config, file, content, ...definedAccess(access) };
}

export function attachConfigCommand(environment: EnvironmentRef, service: string, config: string, dir: string):
  { command: "attach_config" } & AttachConfig {
  return { command: "attach_config", environment, service, config, dir };
}

/** One Save: every edited file of a Config in one batch, so they stage as one change or not at all. */
export function saveConfigCommand(environment: EnvironmentRef, config: string,
  edits: readonly ({ file: string } & FileDraft)[]): ConfigCommand {
  return { command: "batch", environment, commands: edits.map(({ file, content, ...access }) => putConfigFileCommand(environment, config, file, content, access)) };
}

/** The Executable toggle flips only root-owned 0444 and 0555; any other access is custom and shows its owner too. */
export function executableToggle(access: FileAccess): { executable: boolean } | null {
  return access.uid === 0 && access.gid === 0 && (access.mode === READ_ONLY_MODE || access.mode === EXECUTABLE_MODE)
    ? { executable: access.mode === EXECUTABLE_MODE }
    : null;
}

export function fileAccessLabel(access: FileAccess) {
  return executableToggle(access) ? access.mode : `${access.mode} · ${access.uid}:${access.gid}`;
}

/** The largest user or group ID the Store accepts: a u32. */
const MAX_ID = 4_294_967_295;

const modeText = Schema.Trim.check(Schema.makeFilter<string>((mode) => /^0?[0-7]{3}$/u.test(mode)
  ? undefined : "Mode is three octal digits, like 0644.")).pipe(
  Schema.decodeTo(Schema.String, {
    decode: SchemaGetter.transform((mode) => mode.padStart(4, "0")),
    encode: SchemaGetter.transform((mode) => mode),
  }),
);

const idText = (label: string) => Schema.Trim.check(Schema.makeFilter<string>((id) => /^\d{1,10}$/u.test(id) && Number(id) <= MAX_ID
  ? undefined : `${label} is a whole number, like 1000.`)).pipe(
  Schema.decodeTo(Schema.Int, {
    decode: SchemaGetter.transform(Number),
    encode: SchemaGetter.transform(String),
  }),
);

/** The Permissions dialog's three text fields, parsed into a file's access; encoding a file's access prefills them. */
export const fileAccessDraftSchema = Schema.Struct({ mode: modeText, uid: idText("User ID"), gid: idText("Group ID") });

export type FileAccessDraft = typeof fileAccessDraftSchema.Encoded;

/** Bytes as the Size row says them: 812 B, 12.4 KB. */
export function configBytesText(bytes: number) {
  return bytes < 1024 ? `${bytes} B` : `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`;
}

/** A file's text in bytes as the Store counts them: UTF-8. */
export function utf8Bytes(text: string) {
  return new TextEncoder().encode(text).length;
}

/** Why a file's text won't save, before the Store says so: the 256 KB cap. */
export function configFileSizeError(bytes: number) {
  return bytes > CONFIG_FILE_MAX_BYTES ? `Over the 256 KB limit (${configBytesText(bytes)}).` : null;
}

/** A Config under a Service that mounts it, at `dir`; `mountChanged`: the next Deploy adds or moves this mount. */
export type MountedConfig = { config: ConfigListing; dir: string; mountChanged: boolean };

/**
 * Each Config as a tray under every Service here that mounts it, in the Store's order, keyed by Service id, marked
 * where `diff` stages that Service's mount of it; `unmounted`, the Configs no Service here mounts, which are nodes.
 */
export function configTrays(services: readonly Pick<ServiceListing, "id" | "name">[], configs: readonly ConfigListing[], diff: DiffView) {
  const names = new Set(services.map((service) => service.name));
  return {
    trays: new Map(services.map((service) => {
      const changes = serviceChanges(diff, service.id);
      return [service.id, configs.flatMap((config): MountedConfig[] => config.mounts
        .filter((mount) => mount.service === service.name)
        .map((mount) => ({ config, dir: mount.dir, mountChanged: changes.has(`configs.@${config.id}`) })))];
    })),
    unmounted: configs.filter((config) => !config.mounts.some((mount) => names.has(mount.service))),
  };
}

/** Detached rows belong to the exact Config, even when a replacement shares its name. */
export function detachedConfigMounts(diff: DiffView, configId: string) {
  return diff.changes.flatMap((node) => node.type !== "service" ? [] : node.settings.flatMap((row) =>
    row.path === `${node.name}.configs.@${configId}` && row.after === null && row.before !== null
      ? [{ serviceId: node.id, service: node.name, directory: settingText(row.before) }]
      : []));
}

/** A file's unsaved text and whichever access fields the user changed. */
export type FileDraft = { content: string } & Partial<FileAccess>;

/** A file's access as the drawer shows it: its draft's fields over the stored ones, a new file's defaults under both. */
export function draftAccess(stored: FileAccess | undefined, draft: FileDraft | undefined): FileAccess {
  return { ...DEFAULT_ACCESS, ...definedAccess(stored ?? {}), ...definedAccess(draft ?? {}) };
}

const ACCESS_FIELDS = ["mode", "uid", "gid"] as const;

/** The fields of `after` that differ from `before`, so a stale prefill never overwrites an owner set elsewhere. */
export function changedAccess(before: FileAccess, after: FileAccess): Partial<FileAccess> {
  return definedAccess({
    mode: after.mode === before.mode ? undefined : after.mode,
    uid: after.uid === before.uid ? undefined : after.uid,
    gid: after.gid === before.gid ? undefined : after.gid,
  });
}

function definedAccess({ mode, uid, gid }: Partial<FileAccess>): Partial<FileAccess> {
  return { ...(mode !== undefined && { mode }), ...(uid !== undefined && { uid }), ...(gid !== undefined && { gid }) };
}

/** A stored file whose contents the viewer may not read: Ployz support, while its Organization keeps Config files hidden. */
export const hiddenFile = (config: Pick<ConfigItemView, "files" | "contents">, file: string) =>
  config.files.some((one) => one.name === file) && !Object.hasOwn(config.contents, file);

/**
 * What one Save writes: each drafted file whose text or access differs from the Store's, that is new, or whose last save
 * failed (a failed rollback read can leave the optimistic contents cached). A hidden file is written only once it has new
 * text, so neither an access change nor a retry ever empties it.
 */
export function configEdits(
  config: Pick<ConfigItemView, "files" | "contents">, drafts: ReadonlyMap<string, FileDraft>, failed: ReadonlySet<string> = new Set(),
) {
  return [...drafts].flatMap(([file, draft]) => {
    if (hiddenFile(config, file) && draft.content === "") return [];
    const stored = config.files.find((one) => one.name === file);
    const changed = stored === undefined || failed.has(file) || config.contents[file] !== draft.content
      || ACCESS_FIELDS.some((field) => draft[field] !== undefined && draft[field] !== stored[field]);
    return changed ? [{ file, ...draft }] : [];
  });
}

/** Preview's values by `service.KEY`: each Service's built-in exports, then its own variables over them; secrets carry no value. */
export function configReferenceValues(view: EnvironmentView, services: readonly ServiceListing[]) {
  const values = new Map<string, ReferenceValue>();
  for (const service of services) {
    for (const managed of storeManagedExports(service, view.environment)) {
      values.set(`${service.name}.${managed.key}`, { secret: false, value: managed.value });
    }
    for (const variable of serviceVariables(serviceSettingRows(view, service.name), service.id)) {
      values.set(`${service.name}.${variable.key}`,
        variable.value.type === "plain" ? { secret: false, value: variable.value.value } : { secret: true });
    }
  }
  return values;
}
