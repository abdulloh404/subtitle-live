#!/usr/bin/env bash
# หยุดทันทีเมื่อคำสั่งล้มเหลว ใช้ตัวแปรที่ยังไม่ประกาศ หรือ pipeline ส่วนใดผิดพลาด
set -euo pipefail

# เก็บโมเดลไว้ใน directory ส่วนตัวของผู้ใช้โดยไม่ขึ้นกับ XDG_DATA_HOME
model_base_dir="${HOME}/.subtitle-live/models"
model_path="${model_base_dir}/ggml-small.en.bin"
temporary_path="${model_path}.download"
model_url="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin"
model_sha256="c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d"

# สร้าง directory ปลายทางก่อนตรวจหรือดาวน์โหลดไฟล์
mkdir -p "${model_base_dir}"

# ไม่ดาวน์โหลดซ้ำเมื่อมีโมเดลอยู่แล้ว
if [[ -f "${model_path}" ]]; then
    printf 'Model already exists: %s\n' "${model_path}"
    exit 0
fi

# ดาวน์โหลดเป็นไฟล์ชั่วคราว ตรวจ SHA-256 แล้วจึงย้ายเป็นชื่อจริงแบบ atomic ภายใน directory เดียวกัน
printf 'Downloading small.en model (about 488 MB)...\n'
curl --fail --location --retry 3 --output "${temporary_path}" "${model_url}"
printf '%s  %s\n' "${model_sha256}" "${temporary_path}" | sha256sum --check --status
mv "${temporary_path}" "${model_path}"
printf 'Model installed: %s\n' "${model_path}"
