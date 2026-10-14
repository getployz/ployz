import { Schema } from "effect";

export const RawEditorMode = Schema.Literals(["env", "json"]);
export type RawEditorMode = typeof RawEditorMode.Type;
