import { Option, Schema } from "effect";
import { eventType, staticSchema } from "inngest";

// oxlint-disable-next-line anti-slop/no-unsafe-dictionary-type -- the shape Inngest's `eventType` takes; each event's schema narrows it
type EventSchema = Schema.ConstraintDecoder<Record<string, unknown>>;
type EventData = EventSchema["Type"];

function describeEvent<const Name extends string, S extends EventSchema>(name: Name, schema: S) {
  const decode = Schema.decodeUnknownOption(schema);
  return {
    name,
    schema,
    eventType: eventType(name, { schema: staticSchema<S["Type"]>() }),
    /** The event's data, or none when it doesn't match the schema. A property the schema doesn't name is kept. */
    // oxlint-disable-next-line anti-slop/no-unknown-parameters -- the event payload is untrusted; the schema decodes it here
    decode: (input: unknown): Option.Option<S["Type"]> => decode(input, { onExcessProperty: "preserve" }),
  };
}

type Described<Name extends string, S extends EventSchema> = ReturnType<typeof describeEvent<Name, S>>;

/**
 * An event written once: its name, the `eventType` a function triggers on, the schema its data decodes with and the
 * factory that builds it to send. With `id`, Inngest dedupes the event by that key, so sending it again for the same
 * request starts nothing new. Without `id`, every send is a new event.
 */
export function defineEvent<const Name extends string, S extends EventSchema>(definition: {
  readonly name: Name;
  readonly schema: S;
  readonly id: (data: S["Type"]) => string;
}): Described<Name, S> & { readonly create: (data: S["Type"]) => { readonly id: string; readonly name: Name; readonly data: S["Type"] } };
export function defineEvent<const Name extends string, S extends EventSchema>(definition: {
  readonly name: Name;
  readonly schema: S;
}): Described<Name, S> & { readonly create: (data: S["Type"]) => { readonly name: Name; readonly data: S["Type"] } };
export function defineEvent(definition: {
  readonly name: string;
  readonly schema: EventSchema;
  readonly id?: (data: EventData) => string;
}) {
  const { name, schema, id } = definition;
  return {
    ...describeEvent(name, schema),
    create: (data: EventData) => (id === undefined ? { name, data } : { id: id(data), name, data }),
  };
}

/** The data a handler runs on, or none for an event that doesn't match, which the handler answers as `skipped`. */
// oxlint-disable-next-line anti-slop/no-unknown-parameters -- the signature of a decoder built by `defineEvent`
export const eventData = <Data>(definition: { readonly decode: (input: unknown) => Option.Option<Data> }) =>
  (event: { readonly data: unknown }): Option.Option<Data> => definition.decode(event.data);
