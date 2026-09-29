import type { ServiceSettingInput } from './generated/payloads';
export type * from './generated/payloads';

export function parseServiceSetting<Field extends ServiceSettingInput['field']>(field: Field, value: unknown): Extract<ServiceSettingInput, { field: Field }>['value'];
