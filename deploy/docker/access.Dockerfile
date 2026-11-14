# Build WhaleRein Access as a small static Go binary.
FROM golang:1.27-alpine AS build
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
FROM alpine:3.22
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
RUN addgroup -S whalerein && adduser -S -G whalerein whalerein
COPY --from=build /out/whalerein-access /usr/local/bin/whalerein-access
USER whalerein
EXPOSE 8080 8081
ENTRYPOINT ["whalerein-access"]
CMD ["--config", "/etc/whalerein/access.yaml"]
