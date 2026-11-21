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

# Render the broker PUBLISH ceiling from the deployment environment (default
# 1 MiB, matching brokerPacketLimitBytes). The source configuration stays
# immutable in the image; the rendered copy is the one mosquitto reads. Reject
# a non-numeric or non-positive value explicitly. Mosquitto's MQTT 5 Receive
# Maximum is negotiated per connection and is not configured here.
max_packet_size="${WHALEREIN_MQTT_MAX_PACKET_SIZE:-1048576}"
case "$max_packet_size" in
  ''|*[!0-9]*)
    echo "WHALEREIN_MQTT_MAX_PACKET_SIZE must be a positive integer" >&2
    exit 1
    ;;
esac
if [ "$max_packet_size" -le 0 ]; then
  echo "WHALEREIN_MQTT_MAX_PACKET_SIZE must be a positive integer" >&2
  exit 1
fi
cp /mosquitto/config/mosquitto.conf /run/whalerein/mosquitto.conf
sed -i "s/^max_packet_size .*/max_packet_size ${max_packet_size}/" /run/whalerein/mosquitto.conf

exec "$@"
