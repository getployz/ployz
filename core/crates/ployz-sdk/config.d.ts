import type { ServiceSettingInput } from './generated/payloads';
export type * from './generated/payloads';

export function parseServiceSetting<Field extends ServiceSettingInput['field']>(field: Field, value: unknown): Extract<ServiceSettingInput, { field: Field }>['value'];

/** One `.env` reader, shared with `ployz set --from-env-file`. Throws an Error whose message names the first bad line. */
export function parseEnvFile(text: string): Array<{ key: string; value: string }>;
