import type { ServiceSettingInput } from './generated/payloads';
export type * from './generated/payloads';

export function parseServiceSetting<Field extends ServiceSettingInput['field']>(field: Field, value: unknown): Extract<ServiceSettingInput, { field: Field }>['value'];

/** One `.env` reader, shared with `ployz set --from-env-file`. Throws an Error whose message names the first bad line. */
export function parseEnvFile(text: string): Array<{ key: string; value: string }>;

/** The Engine's one region key rule: a region is 1 to 63 letters, digits, hyphens, underscores and dots. Returns the lowercase key; throws an Error whose message says the rule. */
export function parseRegion(value: string): string;

/** The region a new Environment starts in: the first Server's, by lowest Machine Subnet. `null` when that Server has no region label or none is a region, so the Environment may run in any region. */
export function firstServerRegion(servers: ReadonlyArray<{ subnet: string; region: string | null }>): string | null;

/** The one rule for a region's display name: 1 to 40 characters on one line. Returns it trimmed; throws an Error whose message says the rule. */
export function parseRegionName(value: string): string;
