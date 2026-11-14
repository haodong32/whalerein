# Builds the WhaleRein Mosquitto line image.
#
# Mosquitto is built from the pinned upstream release so the deployment can
# carry the broker-local tick scheduling fix required by deferred CONNECT
# authentication. The plugin is compiled against the matching headers. Only the
# broker, its libraries and the plugin are copied into the runtime image.
# syntax=docker/dockerfile:1
FROM alpine:3.24.4@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf AS build

ARG MOSQUITTO_VERSION=2.1.2

RUN apk add --no-cache build-base ca-certificates cjson-dev curl libedit-dev \
      openssl-dev patch sqlite-dev
WORKDIR /src
RUN curl -fsSLo mosquitto.tar.gz \
      "https://github.com/eclipse-mosquitto/mosquitto/archive/refs/tags/v${MOSQUITTO_VERSION}.tar.gz" && \
    tar xzf mosquitto.tar.gz
COPY deploy/mosquitto/patches /src/patches
RUN cd "/src/mosquitto-${MOSQUITTO_VERSION}" && \
    patch -p0 < /src/patches/loop-tick-schedule.patch && \
    make WITH_TLS=yes WITH_WEBSOCKETS=no WITH_DOCS=no WITH_HTTP_API=no && \
    make WITH_TLS=yes WITH_WEBSOCKETS=no WITH_DOCS=no WITH_HTTP_API=no install

COPY deploy/mosquitto/plugin /src/plugin
RUN make -C /src/plugin clean && make -C /src/plugin \
      CC=cc \
      MOSQUITTO_INCLUDE="/src/mosquitto-${MOSQUITTO_VERSION}/include" \
      CFLAGS="-O2 -Wall -Wextra -Werror -fPIC -std=c11 -D_POSIX_C_SOURCE=200809L -D_DEFAULT_SOURCE"

FROM alpine:3.24.4@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf

RUN apk add --no-cache cjson libcrypto3 libedit libssl3 sqlite-libs && \
    addgroup -g 1883 -S mosquitto && \
    adduser -u 1883 -S -G mosquitto -h /var/empty mosquitto && \
    mkdir -p /mosquitto/config /mosquitto/data /mosquitto/log /run/whalerein && \
    chown -R mosquitto:mosquitto /mosquitto /run/whalerein

COPY --from=build /usr/local/sbin/mosquitto /usr/local/sbin/mosquitto
COPY --from=build /usr/local/lib/libmosquitto.so* /usr/local/lib/
COPY --from=build /src/plugin/whalerein.so /usr/lib/mosquitto/whalerein.so
COPY deploy/mosquitto/mosquitto.conf /mosquitto/config/mosquitto.conf
COPY deploy/docker/mosquitto-entrypoint.sh /usr/local/bin/whalerein-entrypoint.sh
RUN ldconfig /usr/local/lib 2>/dev/null || true && \
    chmod 0755 /usr/local/bin/whalerein-entrypoint.sh && \
    chmod 0644 /mosquitto/config/mosquitto.conf && \
    test -x /usr/local/sbin/mosquitto && test -f /usr/lib/mosquitto/whalerein.so

USER mosquitto
ENTRYPOINT ["/usr/local/bin/whalerein-entrypoint.sh"]
CMD ["/usr/local/sbin/mosquitto", "-c", "/mosquitto/config/mosquitto.conf"]
