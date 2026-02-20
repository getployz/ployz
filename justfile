default:
    @just --list

build:
    go build -o bin/ployz ./cmd/ployz

run *args:
    go run ./cmd/ployz {{args}}

clean:
    rm -rf bin/

test:
    go test ./...

lint:
    go vet ./...

tidy:
    go mod tidy

bootstrap *targets:
    ./scripts/bootstrap-remote.sh {{targets}}

deploy-linux *targets:
    ./scripts/deploy-linux-binary.sh {{targets}}
