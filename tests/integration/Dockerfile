# syntax=docker/dockerfile:1
FROM rust:slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7 AS rust
FROM golang:1.26@sha256:cfa27959837144bf4649802ba38e59daffd0dd2a7994aa83daaf113285b0f551 AS go
FROM node:25-bookworm-slim@sha256:81db02c4b671288a03915da9534dbd54f96d0e7c24d80ccc54f5b36b2e684370 AS node
FROM postgres:18-trixie@sha256:e3d24f6906d0760bf4da0dae872415391ededefad23fd471c47dc53b0ddf3c42 AS postgres

FROM rust AS rust-build
USER root
RUN apt-get update && apt-get install -y --no-install-recommends build-essential ca-certificates git protobuf-compiler libprotobuf-dev pkg-config libssl-dev libdbus-1-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /workspace
ENV CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4
ARG TARGETARCH
COPY Cargo.toml Cargo.lock ./
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
    cargo build --locked --workspace && \
    cargo build --locked --example mqtt_transport_driver -p whalerein-core --features protocol-testing && \
    cargo build --locked --manifest-path tests/integration/drivers/noise/rust-peer/Cargo.toml && \
    cargo build --locked --manifest-path tests/integration/drivers/protobuf/rust-peer/Cargo.toml && \
    mkdir -p /out && \
    cp /cache/target/debug/whalerein /cache/target/debug/whalerein-noise-peer /cache/target/debug/whalerein-proto-peer /cache/target/debug/examples/mqtt_transport_driver /out/

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
    go test -c -o /tmp/store-unit-tests ./access/internal/store && \
    go test -c -o /tmp/server-unit-tests ./access/cmd/server && \
    go test -c -o /tmp/store-support-unit-tests ./access/internal/store/storetest && \
    mkdir -p tests/integration/fixtures && \
    python3 -c "import subprocess,pathlib; root=pathlib.Path('tests/integration/fixtures'); names=lambda p:set(subprocess.check_output([p,'-test.list','^Test'],text=True).splitlines()); [(root/(p+'-cases.txt')).write_text('\\n'.join(sorted(names('/out/'+p+'-tests')-names('/tmp/'+p+'-unit-tests')))) for p in ('store','store-support','server')]"
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

# Runtime contains test executables, not compiler toolchains or build caches.
FROM emqx/emqx:5.8.8@sha256:4593b6e30196754779fa14483332cb0be56c233f1c8a387b1308bec4337c0144 AS runtime
USER root
ENV DEBIAN_FRONTEND=noninteractive PATH=/usr/local/bin:/usr/lib/postgresql/18/bin:$PATH LD_LIBRARY_PATH=/usr/local/lib WHALEREIN_INTEGRATION_CONTAINER=1
RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-pytest python3-rich postgresql postgresql-client protobuf-compiler libprotobuf-dev libnuma1 liburing2 libdbus-1-3 libatomic1 && rm -rf /var/lib/apt/lists/*
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=postgres /usr/lib/postgresql/18 /usr/lib/postgresql/18
COPY --from=postgres /usr/share/postgresql/18 /usr/share/postgresql/18
COPY --from=postgres /usr/share/postgresql/postgresql.conf.sample /usr/share/postgresql/postgresql.conf.sample
COPY --from=postgres /usr/lib/*-linux-gnu/libpq.so* /usr/local/lib/
COPY --from=go-build /out/whalerein-access /out/broker-tests /out/server-tests /out/store-tests /out/store-support-tests /usr/local/bin/
COPY tools/repo/src/repo/__init__.py tools/repo/src/repo/test_timing.py /workspace/tools/repo/src/repo/
ENV PYTHONPATH=/workspace/tools/repo/src
WORKDIR /workspace
COPY --from=node-build /workspace/host/dsh host/dsh
COPY --from=node-build /workspace/host/runtime host/runtime
COPY --from=node-build /workspace/node_modules node_modules
COPY --from=rust-build /out/whalerein target/debug/whalerein
COPY --from=rust-build /out/mqtt_transport_driver target/debug/examples/mqtt_transport_driver
COPY proto/v1 proto/v1
COPY tests/integration tests/integration
COPY tests/fixtures tests/fixtures
COPY deploy/compose/emqx deploy/compose/emqx
COPY --from=rust-build /out/whalerein-noise-peer tests/integration/drivers/noise/rust-peer/target/debug/whalerein-noise-peer
COPY --from=rust-build /out/whalerein-proto-peer tests/integration/drivers/protobuf/rust-peer/target/debug/whalerein-proto-peer
COPY --from=go-build /workspace/tests/integration/fixtures/store-cases.txt /workspace/tests/integration/fixtures/store-support-cases.txt /workspace/tests/integration/fixtures/server-cases.txt tests/integration/fixtures/
COPY --from=go-build /workspace/tests/integration/fixtures/*-descriptions.json tests/integration/fixtures/
RUN mkdir -p access/cmd/server access/internal/broker && node --version && ./target/debug/whalerein --version && /usr/lib/postgresql/18/bin/postgres --version && psql --version
ENTRYPOINT ["timeout", "--signal=INT", "--kill-after=30s", "1800s", "python3", "-m", "pytest", "-c", "tests/integration/pytest.ini", "tests/integration/cases"]
