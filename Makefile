# คำสั่งลัดสำหรับ build, run, ตรวจรูปแบบ และดาวน์โหลดโมเดล
.PHONY: build release run format lint test model install-hooks

# สภาพแวดล้อมทั่วไปใช้ได้กับ CPU และ CUDA; ROCm ต้องเพิ่มค่า hipcc แยกต่างหาก
DEV_ENV = . ./scripts/dev-env.sh
ROCM_ENV = $(DEV_ENV) && . ./scripts/rocm-env.sh
COMPUTE_BACKEND ?= $(shell bash ./scripts/detect-compute-backend.sh)
COMPUTE_ENV = $(DEV_ENV)
COMPUTE_FEATURES =
MODEL ?= small.en

ifeq ($(COMPUTE_BACKEND),cuda)
COMPUTE_FEATURES = --features cuda
else ifeq ($(COMPUTE_BACKEND),rocm)
COMPUTE_ENV = $(ROCM_ENV)
COMPUTE_FEATURES = --features rocm
else ifneq ($(COMPUTE_BACKEND),cpu)
$(error COMPUTE_BACKEND must be one of: cpu, cuda, rocm)
endif

build:
	$(COMPUTE_ENV) && cargo build $(COMPUTE_FEATURES)

# สร้าง optimized binary สำหรับวัดประสิทธิภาพจริง
release:
	$(COMPUTE_ENV) && cargo build --release $(COMPUTE_FEATURES)

# เปิดแอปพร้อม structured debug log ตามค่าใน src/logging.rs
run:
	$(COMPUTE_ENV) && cargo run $(COMPUTE_FEATURES)

# จัดรูปแบบ source Rust ทั้ง workspace
format:
	cargo fmt --all

install-hooks:
	git config core.hooksPath .githooks

lint:
	$(COMPUTE_ENV) && cargo clippy --all-targets $(COMPUTE_FEATURES) -- -D warnings

# รัน unit/integration tests ทุก target
test:
	$(COMPUTE_ENV) && cargo test --all-targets $(COMPUTE_FEATURES)

# ดาวน์โหลดและตรวจ checksum ของ model ที่เลือก
model:
	./scripts/download-model.sh "$(MODEL)"
