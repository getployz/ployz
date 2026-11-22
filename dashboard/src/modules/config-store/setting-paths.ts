/**
 * The Store's Setting paths, spelled once. A node's path is `SERVICE`, `volumes.NAME` or `configs.@ID`; a Setting's path is
 * its node's path and then its name within the node (`web.env.KEY`), and a Setting's name within its Service is `env.KEY`,
 * `mounts.VOLUME` or `configs.@ID`.
 */

const ENV = "env.";
const MOUNTS = "mounts.";
const CONFIGS = "configs.";

type NodeRef = { readonly type: "service" | "volume" | "config"; readonly name: string; readonly id: string };

export const volumeNodePath = (name: string) => `volumes.${name}`;
export const configNodePath = (id: string) => `configs.@${id}`;

/** A node's path in the Store: what Discard names to put the whole node back. */
export const nodePath = (node: NodeRef) => node.type === "volume" ? volumeNodePath(node.name) : node.type === "config" ? configNodePath(node.id) : node.name;

/** A Setting's path within `node`. */
export const settingPathIn = (node: NodeRef, setting: string) => `${nodePath(node)}.${setting}`;

/** A Setting's name within its node, from its full path. */
export const settingWithin = (node: NodeRef, path: string) => path.slice(nodePath(node).length + 1);

/** A variable's Setting name within its Service. */
export const envSetting = (key: string) => `${ENV}${key}`;
export const envPath = (service: string, key: string) => `${service}.${envSetting(key)}`;
export const isEnvSetting = (setting: string) => setting.startsWith(ENV);
export const envKeyOf = (setting: string) => setting.slice(ENV.length);

/** A volume mount's Setting name within its Service. */
export const mountSetting = (volume: string) => `${MOUNTS}${volume}`;
export const mountPath = (service: string, volume: string) => `${service}.${mountSetting(volume)}`;
export const isMountSetting = (setting: string) => setting.startsWith(MOUNTS);
export const mountVolumeOf = (setting: string) => setting.slice(MOUNTS.length);

/** A Config mount's Setting name within its Service. */
export const configMountSetting = (configId: string) => configNodePath(configId);
export const configMountPath = (service: string, configId: string) => `${service}.${configMountSetting(configId)}`;
export const isConfigMountSetting = (setting: string) => setting.startsWith(`${CONFIGS}@`);
export const isConfigSetting = (setting: string) => setting.startsWith(CONFIGS);
export const configSettingOf = (setting: string) => setting.slice(CONFIGS.length);
