import type { ContainerId, MachineId, RowId, ServiceId } from "@ployz/sdk";
import { Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";

const decodeMachineId = Schema.decodeUnknownSync(rustMachineIdSchema);

/** A Machine ID from its 32 hex digits, or from one hex digit repeated 32 times. */
export const machineId = (hex: string): MachineId => decodeMachineId(hex.length === 1 ? hex.repeat(32) : hex);

// SAFETY: core mints these ids and the dashboard never parses them, so any distinct string stands in for one.
const minted = <Id extends string>(value: string) => value as Id;

export const containerId = (value: string) => minted<ContainerId>(value);
export const serviceId = (value: string) => minted<ServiceId>(value);
export const rowId = (value: string) => minted<RowId>(value);
