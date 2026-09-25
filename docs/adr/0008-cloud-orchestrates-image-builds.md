# Cloud orchestrates Image Builds; deployment only delivers

Status: Accepted

Image Builds can run on more than one Builder: the Organization Cluster, or GitHub Actions in the Service's own repository. Keeping the build inside the Engine's `prepare` would make the SDK dispatch GitHub workflows, and it would allow falling through in only one direction (GitHub to the Cluster, never back).

Cloud runs each Image Build as its own workflow step, fanned out at admission and not waiting for the Environment execution slot. Each Image Build walks the Build Order: it moves to the next Builder only when the current one does not start the build in time, and a build that has started never moves. On the Cluster, the Engine picks the Server: the one in the Service's latest Build Receipt first, then spreading the attempt's builds, with no new RPC. A GitHub runner builds with its own cache and pushes the image into the Machine Cloud deploys through, authorised by a single-use Build Grant. Every finished Image Build yields a Build Receipt. Deployment takes the slot and only reuses receipts before Direct Image Transfer.

The SDK keeps no knowledge of GitHub. The CLI's `prepare` still builds whatever has no receipt, so Standalone Clusters are unchanged. Cost: the SDK exposes a per-target build call separate from `prepare`, and build progress now appears before the deploying status.
