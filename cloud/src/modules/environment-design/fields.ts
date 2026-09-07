import { environmentResourceSelectSchema } from "#/modules/environment-design/resources";
import { serviceSelectSchema } from "#/modules/environment-design/services";

export const environmentDesignFields = {
  resource: {
    name: environmentResourceSelectSchema.fields.name,
  },
  service: {
    name: serviceSelectSchema.fields.name,
    preDeployCommand: serviceSelectSchema.fields.preDeployCommand,
    healthcheck: serviceSelectSchema.fields.healthcheck,
    restartPolicy: serviceSelectSchema.fields.restartPolicy,
  },
};
