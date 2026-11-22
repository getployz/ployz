import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";

/** The lowercase hex SHA-256 of `value`: what Cloud stores in place of a secret, and what pairing generations are. */
export const sha256Hex = (value: string | Uint8Array) => createHash("sha256").update(value).digest("hex");
