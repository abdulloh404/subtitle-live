# คำสั่งลัดสำหรับ build, run, ตรวจรูปแบบ และดาวน์โหลดโมเดล
.PHONY: build build-cuda build-rocm release release-cuda release-rocm \
	run run-cuda run-rocm format lint lint-cuda lint-rocm test model

# สภาพแวดล้อมทั่วไปใช้ได้กับ CPU และ CUDA; ROCm ต้องเพิ่มค่า hipcc แยกต่างหาก
DEV_ENV = . ./scripts/dev-env.sh
ROCM_ENV = $(DEV_ENV) && . ./scripts/rocm-env.sh

# สร้าง CPU debug binary ซึ่งเป็นค่าเริ่มต้นของโปรเจกต์
build:
	$(DEV_ENV) && cargo build

build-cuda:
	$(DEV_ENV) && cargo build --features cuda

build-rocm:
	$(ROCM_ENV) && cargo build --features rocm

# สร้าง optimized binary สำหรับวัดประสิทธิภาพจริง
release:
	$(DEV_ENV) && cargo build --release

release-cuda:
	$(DEV_ENV) && cargo build --release --features cuda

release-rocm:
	$(ROCM_ENV) && cargo build --release --features rocm

# เปิดแอปพร้อม structured debug log ตามค่าใน src/logging.rs
run:
	$(DEV_ENV) && cargo run

run-cuda:
	$(DEV_ENV) && cargo run --features cuda

run-rocm:
	$(ROCM_ENV) && cargo run --features rocm

# จัดรูปแบบ source Rust ทั้ง workspace
format:
	cargo fmt --all

# CUDA กับ ROCm เปิดพร้อมกันไม่ได้ จึงแยก lint ตาม build profile
lint:
	$(DEV_ENV) && cargo clippy --all-targets -- -D warnings

lint-cuda:
	$(DEV_ENV) && cargo clippy --all-targets --features cuda -- -D warnings

lint-rocm:
	$(ROCM_ENV) && cargo clippy --all-targets --features rocm -- -D warnings

# รัน unit/integration tests ทุก target
test:
	$(DEV_ENV) && cargo test --all-targets

# ดาวน์โหลดและตรวจ checksum ของโมเดล small.en เริ่มต้น
model:
	./scripts/download-model.sh
