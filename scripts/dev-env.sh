#!/usr/bin/env bash

# ใช้ไฟล์นี้ด้วย `source scripts/dev-env.sh` เพื่อกำหนดค่า build ทั่วไป
if [ -n "${BASH_VERSION:-}" ]; then
  if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    echo "Run this script with: source scripts/dev-env.sh" >&2
    exit 1
  fi
elif [ -n "${ZSH_VERSION:-}" ]; then
  case "${ZSH_EVAL_CONTEXT:-}" in
    *:file) ;;
    *)
      echo "Run this script with: source scripts/dev-env.sh" >&2
      exit 1
      ;;
  esac
fi

# ปล่อยให้ bindgen ค้นหา libclang ของระบบ เว้นแต่ผู้ใช้กำหนด path อย่างชัดเจน
if [ -n "${SUBTITLE_LIVE_LIBCLANG_PATH:-}" ]; then
  export LIBCLANG_PATH="${LIBCLANG_PATH:-${SUBTITLE_LIVE_LIBCLANG_PATH}}"
fi
