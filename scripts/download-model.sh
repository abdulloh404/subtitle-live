#!/usr/bin/env bash
# หยุดทันทีเมื่อคำสั่งล้มเหลว ใช้ตัวแปรที่ยังไม่ประกาศ หรือ pipeline ส่วนใดผิดพลาด
set -euo pipefail

# เก็บโมเดลไว้ใน directory ส่วนตัวของผู้ใช้โดยไม่ขึ้นกับ XDG_DATA_HOME
model_base_dir="${HOME}/.subtitle-live/models"
model_id="${1:-small.en}"

case "${model_id}" in
    small.en)
        model_file="ggml-small.en.bin"
        model_size="488 MB"
        model_sha256="c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d"
        model_revision="80da2d8bfee42b0e836fc3a9890373e5defc00a6"
        ;;
    medium.en)
        model_file="ggml-medium.en.bin"
        model_size="1.53 GB"
        model_sha256="cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356"
        model_revision="80da2d8bfee42b0e836fc3a9890373e5defc00a6"
        ;;
    large-v3-turbo)
        model_file="ggml-large-v3-turbo.bin"
        model_size="1.62 GB"
        model_sha256="1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69"
        model_revision="6034871ec87c84e342efab769d4c5c06cd126db3"
        ;;
    *)
        printf 'Unsupported model: %s\n' "${model_id}" >&2
        printf 'Available models: small.en, medium.en, large-v3-turbo\n' >&2
        exit 2
        ;;
esac

model_path="${model_base_dir}/${model_file}"
temporary_path="${model_path}.download"
model_url="https://huggingface.co/ggerganov/whisper.cpp/resolve/${model_revision}/${model_file}"

# สร้าง directory ปลายทางก่อนตรวจหรือดาวน์โหลดไฟล์
mkdir -p "${model_base_dir}"

# ไม่ดาวน์โหลดซ้ำเมื่อมีโมเดลอยู่แล้ว
if [[ -f "${model_path}" ]]; then
    printf 'Model already exists: %s\n' "${model_path}"
    exit 0
fi

# ดาวน์โหลดเป็นไฟล์ชั่วคราว ตรวจ SHA-256 แล้วจึงย้ายเป็นชื่อจริงแบบ atomic ภายใน directory เดียวกัน
printf 'Downloading %s model (%s)...\n' "${model_id}" "${model_size}"
curl --fail --location --retry 3 --output "${temporary_path}" "${model_url}"
printf '%s  %s\n' "${model_sha256}" "${temporary_path}" | sha256sum --check --status
mv "${temporary_path}" "${model_path}"
printf 'Model installed: %s\n' "${model_path}"
