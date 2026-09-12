import {
  publicationBasisMatches,
  parsePublicationBasis,
  type PublicationBasis,
} from "@ployz/sdk/config";
import { sharedSchema } from "./service-config";

export const environmentSavedStateBasisSchema = sharedSchema(parsePublicationBasis);
export type EnvironmentSavedStateBasis = PublicationBasis;
export const environmentSavedStateBasisMatches = publicationBasisMatches;
