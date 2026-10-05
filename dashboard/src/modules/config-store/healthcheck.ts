import type { JsonValue } from "@ployz/sdk";
import { Option, Schema } from "effect";

/** The check a new replica passes before it takes traffic: GET a path, or run a command in the container. */
export type HealthcheckKind = "path" | "command";

/** A Service's `healthcheck` Setting while on, as the Store shows it: `{path, timeoutSeconds}` or `{command, timeoutSeconds}`. */
export type Healthcheck = { kind: HealthcheckKind; text: string; timeoutSeconds: number };

const decode = Schema.decodeUnknownOption(Schema.Union([
  Schema.Struct({ path: Schema.String, timeoutSeconds: Schema.Number }),
  Schema.Struct({ command: Schema.String, timeoutSeconds: Schema.Number }),
]));

/** The check `value` sets, or null when it is off or isn't one. */
export function healthcheckOf(value: JsonValue | undefined): Healthcheck | null {
  return Option.match(decode(value), {
    onNone: () => null,
    onSome: (shown) => "path" in shown
      ? { kind: "path", text: shown.path, timeoutSeconds: shown.timeoutSeconds }
      : { kind: "command", text: shown.command, timeoutSeconds: shown.timeoutSeconds },
  });
}

/** The value that sets `check`, in the shape the Store shows, so a pending edit reads like a saved one. */
export const healthcheckValue = ({ kind, text, timeoutSeconds }: Healthcheck): JsonValue =>
  kind === "path" ? { path: text, timeoutSeconds } : { command: text, timeoutSeconds };
