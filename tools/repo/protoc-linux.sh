#!/usr/bin/env bash
set -euo pipefail

version=36.2
case "$(uname -m)" in
  x86_64)
    arch=x86_64
    checksum=f6ee22d65361338459208da8bff801c9ca689741cba9187b34990dd45198f864
    ;;
  aarch64)
    arch=aarch_64
    checksum=cb89a125fd3d6c1ee86ee65a2bcbd7c3ac7c3fab1a806e0c466f162882053cb7
    ;;
  *) echo "Unsupported Linux architecture for protoc" >&2; exit 1 ;;
esac

fetch() {
  curl --fail --location --silent --show-error --retry 4 --retry-all-errors \
    --connect-timeout 20 --output "$2" "$1"
  printf '%s  %s\n' "$3" "$2" | sha256sum --check
}

compiler=$(mktemp)
descriptor=$(mktemp)
trap 'rm "$compiler" "$descriptor"' EXIT
fetch "https://repo.maven.apache.org/maven2/com/google/protobuf/protoc/4.${version}/protoc-4.${version}-linux-${arch}.exe" \
  "$compiler" "$checksum"
fetch "https://raw.githubusercontent.com/protocolbuffers/protobuf/v${version}/src/google/protobuf/descriptor.proto" \
  "$descriptor" c96db5406336ebcc82523c1dfb122721986a1f43c4aaed45a4a4ad6996efacc2

mkdir -p "$1/bin" "$1/include/google/protobuf"
install -m 0755 "$compiler" "$1/bin/protoc"
install -m 0644 "$descriptor" "$1/include/google/protobuf/descriptor.proto"
