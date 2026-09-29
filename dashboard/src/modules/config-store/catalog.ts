import catalog from "@ployz/sdk/catalog.json";

type ServiceSettingName = keyof typeof catalog.$defs.service.properties;

/**
 * A Service Setting's label, help text and bounds from the settings catalog: the same words `ployz explain` prints.
 * The catalog is generated from core's Setting types and ships with the SDK.
 */
export function serviceSetting<S extends ServiceSettingName>(name: S): (typeof catalog.$defs.service.properties)[S] {
  return catalog.$defs.service.properties[name];
}
