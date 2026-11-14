#!/bin/sh
# Materializes the broker-local secrets the plugin needs from the compose
# environment. Files live in /tmp so the unprivileged broker user can read them
# without granting it write access to the configuration tree.
set -eu
umask 077

if [ -z "${WHALEREIN_BROKER_PASSWORD:-}" ]; then
  echo "WHALEREIN_BROKER_PASSWORD is required" >&2
  exit 1
fi
if [ -z "${WHALEREIN_INTERNAL_TOKEN:-}" ]; then
  echo "WHALEREIN_INTERNAL_TOKEN is required" >&2
  exit 1
fi

rm -f /run/whalerein/mqtt-access-password /run/whalerein/internal-token
printf '%s' "${WHALEREIN_BROKER_PASSWORD}" > /run/whalerein/mqtt-access-password
printf '%s' "${WHALEREIN_INTERNAL_TOKEN}" > /run/whalerein/internal-token
chmod 0400 /run/whalerein/mqtt-access-password /run/whalerein/internal-token

exec "$@"
