# คำสั่งลัดสำหรับ build, run, ตรวจรูปแบบ และดาวน์โหลดโมเดล
.PHONY: build release run format lint test model

# โหลดค่า ROCm, libclang และ GCC ให้คำสั่ง Cargo ที่ต้องคอมไพล์อัตโนมัติ
DEV_ENV = . ./scripts/dev-env.sh

# สร้าง debug binary สำหรับพัฒนา
build:
	$(DEV_ENV) && cargo build

# สร้าง optimized binary สำหรับวัดประสิทธิภาพจริง
release:
	$(DEV_ENV) && cargo build --release

# เปิดแอปพร้อม structured debug log ตามค่าใน src/logging.rs
run:
	$(DEV_ENV) && cargo run

# จัดรูปแบบ source Rust ทั้ง workspace
format:
	cargo fmt --all

# ตรวจคำเตือน Clippy และถือทุกคำเตือนเป็นข้อผิดพลาด
lint:
	$(DEV_ENV) && cargo clippy --all-targets --all-features -- -D warnings

# รัน unit/integration tests ทุก target
test:
	$(DEV_ENV) && cargo test --all-targets

# ดาวน์โหลดและตรวจ checksum ของโมเดล small.en เริ่มต้น
model:
	./scripts/download-model.sh
