# syntax=docker/dockerfile:1
FROM rust:1.100.0-slim-trixie@sha256:e9d95b74296f7126a83ae3d041aeaba1ff4c478f13abc5cc034fcf7c131c3d4a AS rust
FROM golang:1.27.4-trixie@sha256:b54b2639de8c1bfe538ec5ffc45f29b1932235ac5f6d63c059853646677112d9 AS go
FROM node:26.13.1-trixie-slim@sha256:e6bedef217677b276c591795e169840edd9096dbf13f29df8ef8d1856b8820bc AS node
FROM postgres:18.7-trixie@sha256:9f4fd4aa918f52ae3d6f3b687f1de90d8f867cd4d520021545a9a50c120eae3b AS postgres
FROM python:3.15.2-slim-trixie@sha256:65a65fc7b30b06cb6df4930fd4650697cb23f4c2698ab81fc87e4b031bc36928 AS python
FROM mcr.microsoft.com/playwright:v1.65.1-noble@sha256:1507cbfc550a63a6b7396791ca4f00df17beb56342d2447b3f106c306f525579 AS browsers

# Build the pinned broker and install its matching headers for the product plug-in.
FROM debian:trixie@sha256:9c6e381421d0f48d1b8133e573f33c185ee16cbf505eda9c4a8a4791b25ea8a6 AS mosquitto-build
USER root
ARG MOSQUITTO_VERSION=2.1.4
RUN apt-get update && \
    apt-get install -y --no-install-recommends build-essential ca-certificates curl libcjson-dev libedit-dev libsqlite3-dev libssl-dev patch && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN curl -fsSLo mosquitto.tar.gz \
      "https://github.com/eclipse-mosquitto/mosquitto/archive/refs/tags/v${MOSQUITTO_VERSION}.tar.gz" && \
    tar xzf mosquitto.tar.gz
COPY deploy/mosquitto/patches /src/patches
RUN cd "/src/mosquitto-${MOSQUITTO_VERSION}" && patch -p0 < /src/patches/loop-tick-schedule.patch
RUN make -C /src/mosquitto-${MOSQUITTO_VERSION} WITH_TLS=yes WITH_WEBSOCKETS=no WITH_DOCS=no WITH_HTTP_API=no && \
    make -C /src/mosquitto-${MOSQUITTO_VERSION} WITH_TLS=yes WITH_WEBSOCKETS=no WITH_DOCS=no WITH_HTTP_API=no install && \
    cp -R "/src/mosquitto-${MOSQUITTO_VERSION}/include" /opt/mosquitto-include

FROM rust AS rust-tools
RUN --mount=type=bind,source=tools/repo/linux-rust-packages.txt,target=/tmp/linux-rust-packages.txt \
    apt-get update && \
    xargs -r apt-get install -y --no-install-recommends build-essential ca-certificates curl git libssl-dev < /tmp/linux-rust-packages.txt && \
    rm -rf /var/lib/apt/lists/*
RUN --mount=type=bind,source=tools/repo/protoc-linux.sh,target=/tmp/protoc-linux.sh \
    bash /tmp/protoc-linux.sh /usr/local

FROM rust-tools AS vectors-builder
RUN apt-get update && apt-get install -y --no-install-recommends rsync && \
    rm -rf /var/lib/apt/lists/* && mkdir -m 1777 /cache /mirror
ENV HOME=/cache PATH=/usr/local/go/bin:$PATH CARGO_HOME=/cache/cargo CARGO_TARGET_DIR=/cache/target \
    CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 GOPATH=/cache/go GOCACHE=/cache/go-build
WORKDIR /workspace

FROM vectors-builder AS services-builder
COPY --from=go /usr/local/go /usr/local/go
COPY --from=mosquitto-build /opt/mosquitto-include /opt/mosquitto/include
RUN apt-get update && apt-get install -y --no-install-recommends python3 libcjson-dev && rm -rf /var/lib/apt/lists/*

# Only the locked dependencies are part of the stable platform. The local
# workspace package resolves back into the current read-only test view.
FROM node AS node-dependencies
RUN npm install --global pnpm@12.21.0
WORKDIR /opt/whalerein-node
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml ./
COPY host/dsh/package.json host/dsh/package.json
COPY host/runtime/package.json host/runtime/package.json
RUN --mount=type=cache,id=whalerein-integration-pnpm,target=/cache/pnpm,sharing=locked \
    pnpm install --prod --frozen-lockfile --store-dir /cache/pnpm && \
    ln -s /workspace/host/runtime/src host/runtime/src

# Test-only DSH/browser tools are pinned independently of application packages.
FROM node AS native-tools
WORKDIR /opt/native
COPY tests/integration/native/package.json tests/integration/native/package-lock.json ./
RUN --mount=type=cache,id=whalerein-native-npm,target=/root/.npm,sharing=locked \
    npm ci --ignore-scripts --no-audit --no-fund

FROM python AS vectors
COPY --from=rust-tools /usr/local/bin/protoc /usr/local/bin/protoc
COPY --from=rust-tools /usr/local/include/google /usr/local/include/google
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node-dependencies /opt/whalerein-node /opt/whalerein-node
RUN apt-get update && apt-get install -y --no-install-recommends libatomic1 && rm -rf /var/lib/apt/lists/*
RUN python3 -m pip install --no-cache-dir pytest==9.2.0
ENV PYTHONPATH=/workspace/tools/repo/src WHALEREIN_INTEGRATION_CONTAINER=1 PYTHONDONTWRITEBYTECODE=1
WORKDIR /workspace

FROM postgres AS services
USER root
COPY --from=vectors /usr/local /usr/local
COPY --from=vectors /opt/whalerein-node /opt/whalerein-node
ENV DEBIAN_FRONTEND=noninteractive PATH=/usr/local/bin:/usr/lib/postgresql/18/bin:$PATH \
    LD_LIBRARY_PATH=/usr/local/lib WHALEREIN_INTEGRATION_CONTAINER=1 \
    PYTHONPATH=/workspace/tools/repo/src PYTHONDONTWRITEBYTECODE=1
RUN apt-get update && apt-get install -y --no-install-recommends \
    openssl libcjson1 libssl3t64 libnuma1 liburing2 libdbus-1-3 libatomic1 \
    dbus-daemon dbus-bin gnome-keyring libsecret-tools iproute2 && rm -rf /var/lib/apt/lists/*
COPY --from=mosquitto-build /usr/local/sbin/mosquitto /usr/local/sbin/mosquitto
COPY --from=mosquitto-build /usr/local/lib/libmosquitto.so* /usr/local/lib/
RUN ldconfig && test -x /usr/local/sbin/mosquitto
WORKDIR /workspace
ENTRYPOINT []

FROM services AS native
COPY --from=native-tools /opt/native /opt/native
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/playwright-browsers WHALEREIN_CHROMIUM_PATH=/usr/bin/chromium
ENV PATH=/opt/native/node_modules/.bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends chromium \
    && node /opt/native/node_modules/playwright/cli.js install-deps chromium \
    && rm -rf /var/lib/apt/lists/*
COPY --from=browsers /ms-playwright/chromium_headless_shell-1255 /opt/playwright-browsers/chromium_headless_shell-1255
