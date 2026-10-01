import type { Change, JsonValue } from "@ployz/sdk";
import catalog from "@ployz/sdk/catalog.json";

export type ServiceSettingName = keyof typeof catalog.$defs.service.properties;

/** The part of a scalar Setting's JSON Schema a form field needs. */
export type SettingSchema = {
  title: string;
  description: string;
  type: string;
  default?: JsonValue;
  enum?: string[];
  minimum?: number;
  maximum?: number;
  exclusiveMinimum?: number;
  minLength?: number;
  maxLength?: number;
  pattern?: string;
};

/**
 * A Service Setting's label, help text and bounds from the settings catalog: the same words `ployz explain` prints.
 * The catalog is generated from core's Setting types and ships with the SDK.
 */
export function serviceSetting<S extends ServiceSettingName>(name: S): (typeof catalog.$defs.service.properties)[S] {
  return catalog.$defs.service.properties[name];
}

/** A Setting's title by name, for rows that name any Setting (a diff); none for names outside the catalog. */
export function settingTitle(name: string): string | undefined {
  const properties: Record<string, { title?: string }> = catalog.$defs.service.properties;
  return properties[name]?.title;
}

function bounds({ minimum, exclusiveMinimum, maximum }: SettingSchema) {
  const low = minimum !== undefined ? `from ${minimum}` : exclusiveMinimum !== undefined ? `above ${exclusiveMinimum}` : null;
  const high = maximum === undefined ? null : minimum !== undefined ? `to ${maximum}` : `up to ${maximum}`;
  const parts = [low, high].filter((part) => part !== null);
  return parts.length === 0 ? "" : ` ${parts.join(minimum === undefined ? ", " : " ")}`;
}

/**
 * Why a field's text isn't a value the catalog allows, or null. Blank is always allowed: it unsets the Setting, which
 * restores its default. The Store checks again, and words what only it knows (a branch that doesn't exist).
 */
export function settingError(setting: SettingSchema, raw: string): string | null {
  if (raw === "") return null;
  if (setting.type === "integer" || setting.type === "number") {
    const value = Number(raw);
    const whole = setting.type === "integer";
    const fits = Number.isFinite(value) && (!whole || Number.isInteger(value))
      && (setting.minimum === undefined || value >= setting.minimum)
      && (setting.exclusiveMinimum === undefined || value > setting.exclusiveMinimum)
      && (setting.maximum === undefined || value <= setting.maximum);
    return fits ? null : `Enter a ${whole ? "whole number" : "number"}${bounds(setting)}.`;
  }
  // A code point uses at most two UTF-16 units; reject huge pastes before allocating.
  if (setting.maxLength !== undefined && (raw.length > setting.maxLength * 2 || [...raw].length > setting.maxLength)) return `Use at most ${setting.maxLength} characters.`;
  if (setting.pattern !== undefined && !new RegExp(setting.pattern, "u").test(raw)) return `That isn't a valid ${setting.title.toLowerCase()}.`;
  return null;
}

/** The edit a field's text makes: blank unsets the Setting, numbers go as numbers. */
export function settingChange(path: string, setting: SettingSchema, raw: string): Change {
  if (raw === "") return { op: "unset", path };
  return { op: "set", path, value: setting.type === "integer" || setting.type === "number" ? Number(raw) : raw };
}
