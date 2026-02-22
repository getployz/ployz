default:
    @just --list

build:
    go build -o bin/ployz ./cmd/ployz
    go build -o bin/ployzd ./cmd/ployzd

run *args:
    go run ./cmd/ployz {{args}}

clean:
    rm -rf bin/

test:
    go test ./...

lint:
    go vet ./...
    golangci-lint run ./...

tidy:
    go mod tidy

proto:
    protoc --go_out=. --go_opt=paths=source_relative --go-grpc_out=. --go-grpc_opt=paths=source_relative \
        --proto_path=. internal/daemon/pb/daemon.proto

bootstrap *targets:
    ./scripts/bootstrap-remote.sh {{targets}}

deploy *targets:
    ./scripts/deploy-linux-binary.sh {{targets}}

deploy-linux *targets:
    ./scripts/deploy-linux-binary.sh {{targets}}
