#!/usr/bin/env bash
set -euo pipefail

model_base_dir="${XDG_DATA_HOME:-${HOME}/.local/share}/subtitle-live/models"
model_path="${model_base_dir}/ggml-small.en.bin"
temporary_path="${model_path}.download"
model_url="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin"
model_sha256="c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d"

mkdir -p "${model_base_dir}"

if [[ -f "${model_path}" ]]; then
    printf 'Model already exists: %s\n' "${model_path}"
    exit 0
fi

printf 'Downloading small.en model (about 488 MB)...\n'
curl --fail --location --retry 3 --output "${temporary_path}" "${model_url}"
printf '%s  %s\n' "${model_sha256}" "${temporary_path}" | sha256sum --check --status
mv "${temporary_path}" "${model_path}"
printf 'Model installed: %s\n' "${model_path}"

