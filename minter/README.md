# Minter

The minter holds the Ployz GitHub App's private key and client secret, so that hosted Cloud does not. Cloud asks it for installation tokens and gets only `contents`, `metadata`, `pull_requests` and `checks` at `read`. Everything else Cloud does in GitHub is a narrow operation on the build workflow or the Ployz check. No route returns a write token, takes a GitHub path from the caller, or returns Actions jobs, logs or artifacts.

The minter authenticates Cloud but does not trust it. Every response is one a compromised Cloud may hold. The design and its check against the Cloud security bar are in `docs/agents/plans/security-0.4/m-plan.md`.

## Routes

Every route is `POST` with a JSON body. A body with an unknown field is refused.

| Route | Does |
| --- | --- |
| `/v1/token` | Mints an installation token in the read set, for the whole installation or up to 100 repositories |
| `/v1/installation` | Returns an installation's URL and permissions |
| `/v1/workflow` | Says whether `ployz-build.yml` on the default branch is active and canonical |
| `/v1/runs` | Lists the build workflow's runs for a commit and event, metadata only |
| `/v1/run` | Reads one build workflow run, metadata only |
| `/v1/dispatch` | Dispatches `ployz-build.yml` on a branch, only when the file at the branch head is canonical |
| `/v1/cancel` | Cancels a build workflow run |
| `/v1/check` | Creates or updates the Ployz check on a commit |
| `/v1/connect` | Exchanges a connect code and returns the person's login and installations, never their token |

`src/policy.ts` and `src/routes.ts` are the whole security argument. Read them before you change either.

A canonical template is a `ployz-build.yml` whose SHA-256, after CRLF becomes LF, is in `src/canonical-workflows.json`. The dashboard generates the templates, and the file holds their digests. Until the dashboard offers a template that pins the builder by commit, the file is empty and every dispatch is refused.

Each request writes one JSON audit line to stdout: who asked, what they asked for, what GitHub granted, and why a request was refused. It never holds a token, a code, a secret or a request header.

## Run it locally

The local entry listens on loopback, signs with a PEM file, and takes a bearer secret from a file in place of IAM. It is for tests and lab lanes only.

```sh
pnpm install --frozen-lockfile
pnpm build
MINTER_APP_ID=<app id> \
MINTER_APP_KEY_FILE=<pem file, mode 600> \
MINTER_APP_CLIENT_ID=<client id> \
MINTER_APP_CLIENT_SECRET_FILE=<file, mode 600> \
MINTER_BEARER_FILE=<file, mode 600> \
MINTER_CLOUD_ORIGIN=<Cloud origin> \
node dist/local.js
```

`MINTER_PORT` defaults to 8787. `MINTER_CANONICAL_FILE` points at a `{ "digests": [...] }` file, so a lane can dispatch a template that pins a lab builder fork. `MINTER_GITHUB_API` and `MINTER_GITHUB_WEB` point at a stub GitHub.

## Deploy

The operator deploys the minter. No CI job does. It runs as an AWS Lambda behind a Function URL with `AWS_IAM` auth, in an AWS account used for nothing else, from a reviewed commit of this directory. The handler is `dist/lambda.handler`.

| Variable | Holds |
| --- | --- |
| `MINTER_KMS_KEY_ID` | The KMS key whose material is the App's private key (`RSA_2048`, `SIGN_VERIFY`) |
| `GITHUB_APP_ID` | The App ID |
| `GITHUB_APP_CLIENT_ID` | The App's client ID |
| `GITHUB_APP_CLIENT_SECRET` | The App's client secret |
| `MINTER_CLOUD_ORIGIN` | Cloud's origin, which the minter puts in every dispatch's `cloud` input |

The Lambda role may call `kms:Sign` on that key and nothing else. Cloud's IAM user may call `lambda:InvokeFunctionUrl` on the function and nothing else. The Lambda entry signs only with KMS, dispatches only the templates in `src/canonical-workflows.json`, and refuses any request that IAM did not authenticate.

## Test

```sh
pnpm typecheck
pnpm test
```
