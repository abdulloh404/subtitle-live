# คำสั่งลัดสำหรับ build, run, ตรวจรูปแบบ และดาวน์โหลดโมเดล
.PHONY: build release run format lint test model install-extension

# สร้าง debug binary สำหรับพัฒนา
build:
	cargo build

# สร้าง optimized binary สำหรับวัดประสิทธิภาพจริง
release:
	cargo build --release

# เปิดแอปพร้อม structured debug log ตามค่าใน src/logging.rs
run:
	cargo run

# จัดรูปแบบ source Rust ทั้ง workspace
format:
	cargo fmt --all

# ตรวจคำเตือน Clippy และถือทุกคำเตือนเป็นข้อผิดพลาด
lint:
	cargo clippy --all-targets --all-features -- -D warnings

# รัน unit/integration tests ทุก target
test:
	cargo test --all-targets

# ดาวน์โหลดและตรวจ checksum ของโมเดล small.en เริ่มต้น
model:
	./scripts/download-model.sh

# ติดตั้งและเปิด GNOME Shell overlay สำหรับ user ปัจจุบัน
install-extension:
	./scripts/install-gnome-extension.sh
