# GitHub token fixtures

- `token.jwt` is a real GitHub Actions OIDC token, with its signature intact. It comes from run 42597370963 of `.github/workflows/oidc-probe.yml` in the lab repository `Ployz-test/lab-service` (repository ID 1450985696), a `workflow_dispatch` on `main` at `54dfcf4a46870fa09f5d5d6c965dd705c1a24e5c`. Its audience is the dummy `ployz-d2-fixture-dummy`. The job uploaded it only after its `exp` had passed, so it was never usable outside the job.
- `jwks.json` is GitHub's key set as that job fetched it, right after the token was issued.
- `claims.json` is the token's payload, decoded.
- `test-signer.pk8` is an RSA-2048 key in PKCS#8 DER, made with `openssl genpkey` for these tests. It signs test tokens and nothing else.
