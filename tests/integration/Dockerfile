# syntax=docker/dockerfile:1
FROM rust:1.100.0-slim-trixie@sha256:e9d95b74296f7126a83ae3d041aeaba1ff4c478f13abc5cc034fcf7c131c3d4a AS rust
FROM golang:1.27.4-trixie@sha256:b54b2639de8c1bfe538ec5ffc45f29b1932235ac5f6d63c059853646677112d9 AS go
FROM node:26.13.1-trixie-slim@sha256:e6bedef217677b276c591795e169840edd9096dbf13f29df8ef8d1856b8820bc AS node
FROM postgres:18.7-trixie@sha256:9f4fd4aa918f52ae3d6f3b687f1de90d8f867cd4d520021545a9a50c120eae3b AS postgres
FROM python:3.15.2-slim-trixie@sha256:65a65fc7b30b06cb6df4930fd4650697cb23f4c2698ab81fc87e4b031bc36928 AS python
FROM mcr.microsoft.com/playwright:v1.65.1-noble@sha256:1507cbfc550a63a6b7396791ca4f00df17beb56342d2447b3f106c306f525579 AS browsers

# Build the pinned Mosquitto 2.1.2 broker and the WhaleRein plugin against its
# headers. Debian trixie matches the runtime distribution, so the built
# binaries link against the same glibc and OpenSSL ABI as the runtime image.
FROM debian:trixie@sha256:9c6e381421d0f48d1b8133e573f33c185ee16cbf505eda9c4a8a4791b25ea8a6 AS mosquitto-build
USER root
ARG MOSQUITTO_VERSION=2.1.2
RUN apt-get update && \
    apt-get install -y --no-install-recommends build-essential ca-certificates curl libcjson-dev libedit-dev libsqlite3-dev libssl-dev patch && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN curl -fsSLo mosquitto.tar.gz \
      "https://github.com/eclipse-mosquitto/mosquitto/archive/refs/tags/v${MOSQUITTO_VERSION}.tar.gz" && \
    tar xzf mosquitto.tar.gz
COPY deploy/mosquitto/patches /src/patches
RUN cd "/src/mosquitto-${MOSQUITTO_VERSION}" && patch -p0 < /src/patches/loop-tick-schedule.patch
COPY deploy/mosquitto/plugin /src/plugin
RUN make -C /src/mosquitto-${MOSQUITTO_VERSION} WITH_TLS=yes WITH_WEBSOCKETS=no WITH_DOCS=no WITH_HTTP_API=no && \
    make -C /src/mosquitto-${MOSQUITTO_VERSION} WITH_TLS=yes WITH_WEBSOCKETS=no WITH_DOCS=no WITH_HTTP_API=no install && \
    make -C /src/plugin clean && \
    make -C /src/plugin \
      MOSQUITTO_INCLUDE="/src/mosquitto-${MOSQUITTO_VERSION}/include" \
      CFLAGS="-O2 -Wall -Wextra -Werror -fPIC -std=c11 -D_POSIX_C_SOURCE=200809L -D_DEFAULT_SOURCE"

FROM rust AS rust-build
USER root
RUN --mount=type=bind,source=tools/repo/linux-rust-packages.txt,target=/tmp/linux-rust-packages.txt \
    apt-get update && \
    xargs -r apt-get install -y --no-install-recommends build-essential ca-certificates curl git libssl-dev < /tmp/linux-rust-packages.txt && \
    rm -rf /var/lib/apt/lists/*
RUN --mount=type=bind,source=tools/repo/protoc-linux.sh,target=/tmp/protoc-linux.sh \
    bash /tmp/protoc-linux.sh /usr/local
WORKDIR /workspace
ENV CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4
ARG TARGETARCH
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY core core
COPY apps/cli apps/cli
COPY proto/v1/whalerein.proto proto/v1/whalerein.proto
COPY proto/v1/bindings/rust proto/v1/bindings/rust
COPY tools/noise-peer tools/noise-peer
COPY tests/integration/drivers/noise/rust-peer tests/integration/drivers/noise/rust-peer
COPY tests/integration/drivers/protobuf/rust-peer tests/integration/drivers/protobuf/rust-peer
RUN --mount=type=cache,id=whalerein-cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=whalerein-cargo-git-${TARGETARCH},target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=whalerein-cargo-target-${TARGETARCH},target=/cache/target,sharing=locked \
    export CARGO_TARGET_DIR=/cache/target && \
    cargo clean -p whalerein-proto && \
    cargo build --locked --workspace --features whalerein-core/protocol-testing && \
    cargo build --locked --example mqtt_transport_driver -p whalerein-core --features protocol-testing && \
    cargo build --locked --example delivery_budget_driver -p whalerein-core --features protocol-testing && \
    cargo build --locked --example wait_concurrency_driver -p whalerein-core && \
    cargo build --locked --manifest-path tests/integration/drivers/noise/rust-peer/Cargo.toml && \
    cargo build --locked --manifest-path tests/integration/drivers/protobuf/rust-peer/Cargo.toml && \
    mkdir -p /out && \
    cp /cache/target/debug/whalerein /cache/target/debug/whalerein-noise-peer /cache/target/debug/whalerein-proto-peer /cache/target/debug/examples/mqtt_transport_driver /cache/target/debug/examples/delivery_budget_driver /cache/target/debug/examples/wait_concurrency_driver /out/

FROM go AS go-build
RUN apt-get update && apt-get install -y --no-install-recommends python3 && rm -rf /var/lib/apt/lists/*
WORKDIR /workspace
ARG TARGETARCH
COPY go.mod go.sum ./
RUN --mount=type=cache,id=whalerein-go-mod,target=/go/pkg/mod,sharing=locked go mod download
COPY access access
COPY proto/v1/bindings/go proto/v1/bindings/go
RUN --mount=type=cache,id=whalerein-go-mod,target=/go/pkg/mod,sharing=locked \
    --mount=type=cache,id=whalerein-go-build-${TARGETARCH},target=/root/.cache/go-build,sharing=locked \
    mkdir -p /out && \
    go build -o /out/whalerein-access ./access/cmd/server && \
    go test -c -tags=integration -o /out/broker-tests ./access/internal/broker && \
    go test -c -tags=integration -o /out/server-tests ./access/cmd/server && \
    go test -c -tags=integration -o /out/store-tests ./access/internal/store && \
    go test -c -tags=integration -o /out/store-support-tests ./access/internal/store/storetest && \
    go test -c -tags=integration -o /out/external-tests ./access/internal/external && \
    go test -c -o /tmp/store-unit-tests ./access/internal/store && \
    go test -c -o /tmp/server-unit-tests ./access/cmd/server && \
    go test -c -o /tmp/store-support-unit-tests ./access/internal/store/storetest && \
    go test -c -o /tmp/external-unit-tests ./access/internal/external && \
    mkdir -p tests/integration/fixtures && \
    python3 -c "import subprocess,pathlib; root=pathlib.Path('tests/integration/fixtures'); names=lambda p:set(subprocess.check_output([p,'-test.list','^Test'],text=True).splitlines()); [(root/(p+'-cases.txt')).write_text('\\n'.join(sorted(names('/out/'+p+'-tests')-names('/tmp/'+p+'-unit-tests')))) for p in ('store','store-support','server','external')]"
COPY tests/integration/drivers/go_descriptions.py tests/integration/drivers/go_descriptions.py
RUN python3 tests/integration/drivers/go_descriptions.py

FROM node AS node-build
RUN npm install --global pnpm@12.18.0
ARG TARGETARCH
WORKDIR /workspace
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml ./
COPY host/runtime/package.json host/runtime/package.json
COPY host/dsh/package.json host/dsh/package.json
RUN --mount=type=cache,id=whalerein-pnpm-${TARGETARCH},target=/cache/pnpm,sharing=locked \
    pnpm install --frozen-lockfile --store-dir /cache/pnpm
COPY host/ host/

# Test-only DSH/browser tools are pinned independently of application packages.
FROM node AS native-tools
WORKDIR /opt/native
COPY tests/integration/native/package.json tests/integration/native/package-lock.json ./
RUN --mount=type=cache,id=whalerein-native-npm,target=/root/.npm,sharing=locked \
    npm ci --ignore-scripts --no-audit --no-fund

# Runtime contains test executables, not compiler toolchains or build caches.
#
# The runtime is the Debian trixie PostgreSQL 18 image already used above:
# trixie matches the Rust build stage and the Mosquitto build stage, and the
# image already owns PostgreSQL 18, the postgres account and the client tools.
# It replaced the former EMQX image, which was also Debian but carried the
# broker being replaced and made the runtime depend on its directory layout.
FROM postgres AS runtime
USER root
COPY --from=python /usr/local /usr/local
ENV DEBIAN_FRONTEND=noninteractive PATH=/usr/local/bin:/usr/lib/postgresql/18/bin:$PATH LD_LIBRARY_PATH=/usr/local/lib WHALEREIN_INTEGRATION_CONTAINER=1
RUN apt-get update && apt-get install -y --no-install-recommends libcjson1 libssl3t64 libnuma1 liburing2 libdbus-1-3 libatomic1 && rm -rf /var/lib/apt/lists/*
RUN python3 -m pip install --no-cache-dir pytest==9.2.0
COPY --from=rust-build /usr/local/bin/protoc /usr/local/bin/protoc
COPY --from=rust-build /usr/local/include/google /usr/local/include/google
COPY --from=mosquitto-build /usr/local/sbin/mosquitto /usr/local/sbin/mosquitto
COPY --from=mosquitto-build /usr/local/lib/libmosquitto.so* /usr/local/lib/
COPY --from=mosquitto-build /src/plugin/whalerein.so /usr/local/lib/mosquitto/whalerein.so
RUN ldconfig && test -x /usr/local/sbin/mosquitto && test -f /usr/local/lib/mosquitto/whalerein.so
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=native-tools /opt/native /opt/native
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/playwright-browsers
ENV PATH=/opt/native/node_modules/.bin:$PATH
RUN node /opt/native/node_modules/playwright/cli.js install-deps chromium \
    && rm -rf /var/lib/apt/lists/*
COPY --from=browsers /ms-playwright/chromium_headless_shell-1255 /opt/playwright-browsers/chromium_headless_shell-1255
COPY --from=go-build /out/whalerein-access /out/broker-tests /out/server-tests /out/store-tests /out/store-support-tests /out/external-tests /usr/local/bin/
COPY tools/repo/src/repo/__init__.py tools/repo/src/repo/test_timing.py /workspace/tools/repo/src/repo/
ENV PYTHONPATH=/workspace/tools/repo/src
WORKDIR /workspace
COPY --from=node-build /workspace/host/dsh host/dsh
COPY --from=node-build /workspace/host/runtime host/runtime
COPY --from=node-build /workspace/node_modules node_modules
COPY --from=rust-build /out/whalerein target/debug/whalerein
COPY --from=rust-build /out/mqtt_transport_driver target/debug/examples/mqtt_transport_driver
COPY --from=rust-build /out/delivery_budget_driver target/debug/examples/delivery_budget_driver
COPY --from=rust-build /out/wait_concurrency_driver target/debug/examples/wait_concurrency_driver
COPY proto/v1 proto/v1
COPY tests/integration tests/integration
COPY tests/fixtures tests/fixtures
COPY --from=rust-build /out/whalerein-noise-peer tests/integration/drivers/noise/rust-peer/target/debug/whalerein-noise-peer
COPY --from=rust-build /out/whalerein-proto-peer tests/integration/drivers/protobuf/rust-peer/target/debug/whalerein-proto-peer
COPY --from=go-build /workspace/tests/integration/fixtures/store-cases.txt /workspace/tests/integration/fixtures/store-support-cases.txt /workspace/tests/integration/fixtures/server-cases.txt /workspace/tests/integration/fixtures/external-cases.txt tests/integration/fixtures/
COPY --from=go-build /workspace/tests/integration/fixtures/*-descriptions.json tests/integration/fixtures/
RUN mkdir -p access/cmd/server access/internal/broker && node --version && ./target/debug/whalerein --version && /usr/lib/postgresql/18/bin/postgres --version && psql --version
# Two entry points share one image: the correctness suite (default) and the
# opt-in benchmark, which never runs as part of the suite.
ENTRYPOINT ["sh", "-c", "if [ \"$1\" = benchmark ]; then shift; exec timeout --signal=INT --kill-after=10s \"${WHALEREIN_BENCHMARK_TIMEOUT:-900}s\" python3 tests/integration/benchmark/main.py; fi; exec timeout --signal=INT --kill-after=30s \"${WHALEREIN_INTEGRATION_TIMEOUT:-7200}s\" python3 -m pytest -c tests/integration/pytest.ini tests/integration/cases \"$@\"", "integration"]
