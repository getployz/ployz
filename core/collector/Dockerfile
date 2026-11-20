FROM golang@sha256:3b4a11519ad929d1e1d261a12cff056f0c85b735253d7d861346b9c6f8b36437 AS build
ENV CGO_ENABLED=0 GOFLAGS="-trimpath -buildvcs=false"
RUN go install go.opentelemetry.io/collector/cmd/builder@v0.147.0
WORKDIR /src
COPY manifest.yaml .
RUN builder --config=manifest.yaml

FROM gcr.io/distroless/static-debian12@sha256:d75cdd72874d4790092fcb1b058493ecf6bb5bf2b2b897045b00ff01d91843f2
COPY --from=build /src/out/otelcol-ployz /otelcol-ployz
ENTRYPOINT ["/otelcol-ployz"]
