#!/usr/bin/env bash
set -euo pipefail

# The runner mounts its stable source mirror read-only at /workspace and
# holds the per-worktree build lock while these commands publish artifacts.
publish() {
    install -D "$1" "/products/$2"
}
cargo_build() {
    cargo build --locked "$@"
}
go_tests() {
    local binary="/products/.verification/bin/$1-tests"
    if [[ ! -f "$binary" ]]; then
        go test -c -tags=integration -o "$binary" "./$2"
    fi
}
for artifact in "$@"; do
    case "$artifact" in
        access)
            mkdir -p /products/.verification/bin
            go build -o /products/.verification/bin/whalerein-access ./access/cmd/server ;;
        broker-plugin)
            mkdir -p /cache/plugin
            rsync -c deploy/mosquitto/plugin/{Makefile,whalerein.c} /cache/plugin
            make -C /cache/plugin MOSQUITTO_INCLUDE=/opt/mosquitto/include
            publish /cache/plugin/whalerein.so .verification/lib/whalerein.so ;;
        cli)
            cargo_build -p whalerein-cli --features whalerein-core/protocol-testing
            publish /cache/target/debug/whalerein target/debug/whalerein ;;
        noise-peer|proto-peer)
            if [[ "$artifact" == noise-peer ]]; then driver=noise; else driver=protobuf; fi
            path="tests/integration/drivers/$driver/rust-peer"
            cargo_build --manifest-path "$path/Cargo.toml"
            publish "/cache/target/debug/whalerein-$artifact" "$path/target/debug/whalerein-$artifact" ;;
        mqtt-driver|delivery-driver|wait-driver)
            case "$artifact" in
                mqtt-driver) example=mqtt_transport_driver ;;
                delivery-driver) example=delivery_driver ;;
                wait-driver) example=wait_concurrency_driver ;;
            esac
            cargo_build --example "$example" -p whalerein-core --features protocol-testing
            publish "/cache/target/debug/examples/$example" "target/debug/examples/$example" ;;
        broker-tests|server-tests)
            mkdir -p /products/.verification/bin
            if [[ "$artifact" == broker-tests ]]; then
                go_tests broker access/internal/broker
            else
                go_tests server access/cmd/server
            fi ;;
        storage-tests)
            mkdir -p /products/.verification/bin /products/tests/integration/fixtures
            python3 tests/integration/drivers/go_descriptions.py --products /products ;;
        *) printf 'Unknown build artifact: %s\n' "$artifact" >&2; exit 2 ;;
    esac
done
