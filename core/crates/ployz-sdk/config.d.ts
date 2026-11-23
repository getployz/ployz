import type { ServiceSettingInput } from './generated/payloads';
export type * from './generated/payloads';

export function parseServiceSetting<Field extends ServiceSettingInput['field']>(field: Field, value: unknown): Extract<ServiceSettingInput, { field: Field }>['value'];

/** One `.env` reader, shared with `ployz set --from-env-file`. Throws an Error whose message names the first bad line. */
export function parseEnvFile(text: string): Array<{ key: string; value: string }>;

/** The Engine's one region key rule: a region is 1 to 63 letters, digits, hyphens, underscores and dots. Returns the lowercase key; throws an Error whose message says the rule. */
export function parseRegion(value: string): string;

/** The region a new Environment starts in: the first Server's, by earliest `joined` time (milliseconds, a Server with none first), then lowest Machine Subnet. Pass every Server, with a `null` subnet for one whose record isn't read. `null` when any Server has no region label that is a region or isn't read, so the Environment may run in any region. */
export function firstServerRegion(servers: ReadonlyArray<{ subnet: string | null; joined?: number | null; region: string | null }>): string | null;

/** The one rule for a region's display name: 1 to 40 characters on one line. Returns it trimmed; throws an Error whose message says the rule. */
export function parseRegionName(value: string): string;

/** What `${{ secret(…) }}` asks for, as the Store reads it: the length and the distinct characters, in ASCII order, to draw it from. Throws an Error whose message says the rule. */
export function parseGenerator(value: string): { length: number; alphabet: string };
