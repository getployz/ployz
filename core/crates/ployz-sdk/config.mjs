import { config_request } from './generated/config-wasm.mjs';

const request = input => JSON.parse(config_request(JSON.stringify(input)));
export const parseServiceSetting = (field, value) => request({ operation: 'parse_setting', value: { field, value } });
export const parseEnvFile = text => request({ operation: 'parse_env_file', text });
export const parseRegion = value => request({ operation: 'parse_region', value });
export const firstServerRegion = servers => request({ operation: 'first_server_region', servers });
export const parseRegionName = value => request({ operation: 'parse_region_name', value });
