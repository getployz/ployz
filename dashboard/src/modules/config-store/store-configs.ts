import type {
  AttachConfig, ConfigCommand, ConfigFileSummary, ConfigItemQuery, ConfigListing, ConfigsQuery, CreateConfig, DiffView, EnvironmentRef,
  PutConfigFile, ServiceListing,
} from "@ployz/sdk";
import { serviceChanges } from "./store-services";

/** The most text one Config file holds, in bytes; the Store's `MAX_FILE_BYTES`. */
export const CONFIG_FILE_MAX_BYTES = 256 * 1024;

/** The mode a Config file gets unless the user marks it executable. */
export const READ_ONLY_MODE = "0444";
export const EXECUTABLE_MODE = "0555";

/** An Environment's Configs with their files (no text) and where Services mount them: the canvas's trays and nodes. */
export function configsQuery(environment: EnvironmentRef): { query: "configs" } & ConfigsQuery {
  return { query: "configs", environment };
}

/** One Config with every file's text, references by Service name: its drawer. */
export function configQuery(environment: EnvironmentRef, config: string): { query: "config" } & ConfigItemQuery {
  return { query: "config", environment, config };
}

/** The command that creates an empty Config with the id the caller minted, mounted nowhere yet. */
export function createConfigCommand(id: string, environment: EnvironmentRef, name: string): { command: "create_config" } & CreateConfig {
  return { command: "create_config", id, environment, name, mounts: [] };
}

/** Writes one file's text; `mode` only when the user flipped Executable, so a CLI-set owner stays. */
export function putConfigFileCommand(environment: EnvironmentRef, config: string, file: string, content: string, mode?: string):
  { command: "put_config_file" } & PutConfigFile {
  return { command: "put_config_file", environment, config, file, content, ...(mode === undefined ? {} : { mode }) };
}

export function attachConfigCommand(environment: EnvironmentRef, service: string, config: string, dir: string):
  { command: "attach_config" } & AttachConfig {
  return { command: "attach_config", environment, service, config, dir };
}

/** One Save: every edited file of a Config in one batch, so they stage as one change or not at all. */
export function saveConfigCommand(environment: EnvironmentRef, config: string,
  edits: readonly { file: string; content: string; mode?: string }[]): ConfigCommand {
  return { command: "batch", environment, commands: edits.map((edit) => putConfigFileCommand(environment, config, edit.file, edit.content, edit.mode)) };
}

/**
 * What the Executable toggle can say about a file: read-only (0444) or executable (0555) as root, which it flips
 * between; anything else was set through the CLI or SDK, and shows as a fixed label.
 */
export type FileAccess =
  | { kind: "toggle"; executable: boolean }
  | { kind: "fixed"; label: string };

export function fileAccess(file: Pick<ConfigFileSummary, "mode" | "uid" | "gid">): FileAccess {
  if (file.uid === 0 && file.gid === 0 && (file.mode === READ_ONLY_MODE || file.mode === EXECUTABLE_MODE)) {
    return { kind: "toggle", executable: file.mode === EXECUTABLE_MODE };
  }
  return { kind: "fixed", label: `${file.mode} · ${file.uid}:${file.gid}` };
}

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
      // A mount is the Service's Setting, `configs.CONFIG`.
      const changes = serviceChanges(diff, service.id);
      return [service.id, configs.flatMap((config): MountedConfig[] => config.mounts
        .filter((mount) => mount.service === service.name)
        .map((mount) => ({ config, dir: mount.dir, mountChanged: changes.has(`configs.${config.name}`) })))];
    })),
    unmounted: configs.filter((config) => !config.mounts.some((mount) => names.has(mount.service))),
  };
}
