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

FROM alpine:3.22
RUN apk add --no-cache ca-certificates wget \
    && addgroup -S whalerein \
    && adduser -S -G whalerein whalerein
COPY --from=build /out/whalerein-access /usr/local/bin/whalerein-access
USER whalerein
EXPOSE 8080 8081
ENTRYPOINT ["whalerein-access"]
CMD ["--config", "/etc/whalerein/access.yaml"]
