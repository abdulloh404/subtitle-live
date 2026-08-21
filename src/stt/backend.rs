//! ข้อมูล backend สำหรับคำนวณที่ถูกบรรจุอยู่ในไบนารีปัจจุบัน

/// backend ที่สามารถรองรับได้เมื่อเพิ่มรูปแบบการ build ในอนาคต
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputeBackend {
    /// เลือก backend ที่เหมาะสมโดยอัตโนมัติ
    Auto,
    /// ใช้ AMD GPU ผ่าน ROCm/HIP
    Rocm,
    /// ใช้ NVIDIA GPU ผ่าน CUDA
    Cuda,
    /// ใช้ GPU ผ่าน Vulkan
    Vulkan,
    /// ใช้ CPU เท่านั้น
    Cpu,
}

impl ComputeBackend {
    /// ชื่อสำหรับแสดงในหน้า Settings และ About
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Rocm => "ROCm / HIP",
            Self::Cuda => "CUDA",
            Self::Vulkan => "Vulkan",
            Self::Cpu => "CPU",
        }
    }
}

/// คืน backend ที่เปิดใช้ตอนคอมไพล์ไบนารีนี้
///
/// ปัจจุบัน `whisper-rs` เปิด feature `hipblas` โดยตรงใน Cargo.toml จึงเป็น
/// ROCm/HIP เสมอ ค่านี้แยกไว้เพื่อให้เปลี่ยนตาม build feature ได้ในอนาคต
pub const fn compiled_compute_backend() -> ComputeBackend {
    ComputeBackend::Rocm
}

#[cfg(test)]
mod tests {
    use super::{ComputeBackend, compiled_compute_backend};

    #[test]
    fn display_labels_cover_supported_backends() {
        assert_eq!(ComputeBackend::Auto.display_name(), "Auto");
        assert_eq!(ComputeBackend::Rocm.display_name(), "ROCm / HIP");
        assert_eq!(ComputeBackend::Cuda.display_name(), "CUDA");
        assert_eq!(ComputeBackend::Vulkan.display_name(), "Vulkan");
        assert_eq!(ComputeBackend::Cpu.display_name(), "CPU");
    }

    #[test]
    fn compiled_backend_matches_current_hipblas_build() {
        assert_eq!(compiled_compute_backend(), ComputeBackend::Rocm);
    }
}
