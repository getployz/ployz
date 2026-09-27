import { expect, it } from "vitest";
import { legacyDeploymentLink } from "./deployment-page";

const id = "00000000-0000-4000-8000-000000000011";
const api = "00000000-0000-4000-8000-000000000021";

it("sends old Deployment Mode links to the Deployment Page, keeping the service and tab they named", () => {
  expect(legacyDeploymentLink({ pathname: "/cloud/acme/shop/production", search: { deployment: id } }))
    .toEqual({ deploymentId: id, search: { service: undefined, logs: undefined } });
  expect(legacyDeploymentLink({ pathname: `/cloud/acme/shop/production/services/${api}`, search: { deployment: id, tab: "build-logs" } }))
    .toEqual({ deploymentId: id, search: { service: api, logs: "build" } });
  // Details has no tab on the page: its stage decides.
  expect(legacyDeploymentLink({ pathname: `/cloud/acme/shop/production/services/${api}`, search: { deployment: id, tab: "details" } }))
    .toEqual({ deploymentId: id, search: { service: api, logs: undefined } });
  expect(legacyDeploymentLink({ pathname: "/cloud/acme/shop/production", search: { deployment: "not-an-id" } })).toBeNull();
  expect(legacyDeploymentLink({ pathname: "/cloud/acme/shop/production", search: {} })).toBeNull();
});
