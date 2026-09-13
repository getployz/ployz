import { Schema } from "effect";

export const Actor = Schema.Struct({
  userId: Schema.String,
});

export type Actor = typeof Actor.Type;
