-- The Free receipt a free Build's Server gave for each image it loaded, under the
-- Namespace and Service that ran it. A copy of a Container running that image, such as a
-- drain's, hands it to its Server, which runs the image unsigned only by a receipt that
-- holds under its Cluster's key. A receipt is a MAC over the image ID that only the
-- Cluster's Servers make, so keeping one grants nothing.
CREATE TABLE config_free_receipt (
    organization_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    service TEXT NOT NULL,
    image TEXT NOT NULL,
    receipt TEXT NOT NULL,
    PRIMARY KEY (organization_id, namespace, service, image)
);
