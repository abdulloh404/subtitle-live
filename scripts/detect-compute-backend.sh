#!/usr/bin/env bash
set -euo pipefail

subtitle_live_nvidia_detected=false
subtitle_live_amd_detected=false

if [[ -e /dev/nvidiactl ]]; then
  subtitle_live_nvidia_detected=true
fi
if [[ -e /dev/kfd ]]; then
  subtitle_live_amd_detected=true
fi

for subtitle_live_device_path in /sys/bus/pci/devices/*; do
  [[ -r "${subtitle_live_device_path}/class" ]] || continue
  [[ -r "${subtitle_live_device_path}/vendor" ]] || continue

  subtitle_live_device_class=$(<"${subtitle_live_device_path}/class")
  case "${subtitle_live_device_class,,}" in
    0x03*) ;;
    *) continue ;;
  esac

  subtitle_live_device_vendor=$(<"${subtitle_live_device_path}/vendor")
  case "${subtitle_live_device_vendor,,}" in
    0x10de) subtitle_live_nvidia_detected=true ;;
    0x1002) subtitle_live_amd_detected=true ;;
  esac
done

if [[ "${subtitle_live_nvidia_detected}" == true ]]; then
  printf '%s\n' cuda
elif [[ "${subtitle_live_amd_detected}" == true ]]; then
  printf '%s\n' rocm
else
  printf '%s\n' cpu
fi
