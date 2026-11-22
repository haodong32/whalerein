# Build WhaleRein Access as a small static Go binary.
FROM golang:1.27.4-alpine3.24@sha256:0a5f7fe796599ff487605987432e7619ae45beeb2fba4c2ce0b907879932aebf AS build
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY access ./access
COPY proto ./proto
RUN CGO_ENABLED=0 GOOS=linux go build \
    -trimpath -ldflags="-s -w" \
    -o /out/whalerein-access ./access/cmd/server

# The runtime needs no package installation: CA trust comes from the build
# stage and BusyBox provides the healthcheck wget applet.
FROM alpine:3.24.4@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
RUN addgroup -S whalerein && adduser -S -G whalerein whalerein && \
    mkdir -p /var/lib/whalerein/payloads && \
    chown -R whalerein:whalerein /var/lib/whalerein
COPY --from=build /out/whalerein-access /usr/local/bin/whalerein-access
USER whalerein
EXPOSE 8080 8081
ENTRYPOINT ["whalerein-access"]
CMD ["--config", "/etc/whalerein/access.yaml"]
