#!/usr/bin/env bash

# ใช้ไฟล์นี้ต่อจาก dev-env.sh เฉพาะ build ที่เปิด feature `rocm`
if [ -n "${BASH_VERSION:-}" ]; then
  if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    echo "Run this script with: source scripts/rocm-env.sh" >&2
    exit 1
  fi
elif [ -n "${ZSH_VERSION:-}" ]; then
  case "${ZSH_EVAL_CONTEXT:-}" in
    *:file) ;;
    *)
      echo "Run this script with: source scripts/rocm-env.sh" >&2
      exit 1
      ;;
  esac
fi

subtitle_live_rocm_core="${SUBTITLE_LIVE_ROCM_CORE:-/opt/rocm/core}"
subtitle_live_hip_gcc_dir="${SUBTITLE_LIVE_HIP_GCC_DIR:-/usr/lib/gcc/x86_64-linux-gnu/11}"

case ":${CMAKE_PREFIX_PATH:-}:" in
  *":${subtitle_live_rocm_core}:"*) ;;
  *) export CMAKE_PREFIX_PATH="${subtitle_live_rocm_core}${CMAKE_PREFIX_PATH:+:${CMAKE_PREFIX_PATH}}" ;;
esac

subtitle_live_hip_gcc_flag="--gcc-install-dir=${subtitle_live_hip_gcc_dir}"
case " ${HIPCC_COMPILE_FLAGS_APPEND:-} " in
  *" --gcc-install-dir="*) ;;
  *) export HIPCC_COMPILE_FLAGS_APPEND="${HIPCC_COMPILE_FLAGS_APPEND:+${HIPCC_COMPILE_FLAGS_APPEND} }${subtitle_live_hip_gcc_flag}" ;;
esac
case " ${HIPCC_LINK_FLAGS_APPEND:-} " in
  *" --gcc-install-dir="*) ;;
  *) export HIPCC_LINK_FLAGS_APPEND="${HIPCC_LINK_FLAGS_APPEND:+${HIPCC_LINK_FLAGS_APPEND} }${subtitle_live_hip_gcc_flag}" ;;
esac

export LIBCLANG_PATH="${LIBCLANG_PATH:-${SUBTITLE_LIVE_LIBCLANG_PATH:-${subtitle_live_rocm_core}/llvm/lib}}"

unset subtitle_live_rocm_core subtitle_live_hip_gcc_dir subtitle_live_hip_gcc_flag
