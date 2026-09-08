import {
  publicationBasisMatches,
  parsePublicationBasis,
  parseSavedDiscard,
  type PublicationBasis,
  type SavedDiscardOperation,
  type SavedDiscardCommand,
} from "@ployz/sdk/config";
import { sharedSchema } from "./service-config";

export const environmentSavedStateBasisSchema = sharedSchema(parsePublicationBasis);
export const environmentSavedStateDiscardCommandSchema = sharedSchema(parseSavedDiscard);
export type EnvironmentSavedStateBasis = PublicationBasis;
export type EnvironmentSavedStateDiscardOperation = SavedDiscardOperation;
export type EnvironmentSavedStateDiscardCommand = SavedDiscardCommand;
export const environmentSavedStateBasisMatches = publicationBasisMatches;
