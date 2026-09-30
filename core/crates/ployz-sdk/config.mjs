import { config_request } from './generated/config-wasm.mjs';

const request = input => JSON.parse(config_request(JSON.stringify(input)));
export const parseServiceSetting = (field, value) => request({ operation: 'parse_setting', value: { field, value } });
