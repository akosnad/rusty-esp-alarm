#!/usr/bin/env bash
set -e

if [ -z "${1}" ]; then
    echo "Usage: $0 <mqtt_endpoint>/<ota_topic>"
    exit 1
fi

cargo build --release
espflash save-image --chip esp32 target/xtensa-esp32-espidf/release/rusty-esp-alarm ota.bin
mosquitto_pub -L "${1}" -f ota.bin -d -q 2
