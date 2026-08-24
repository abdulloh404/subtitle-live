// ตรวจ compile capability และ GPU device ที่ใช้ได้สำหรับ Whisper

use std::sync::OnceLock;

use crate::config::ComputeRequest;

#[cfg(all(feature = "cuda", feature = "rocm"))]
compile_error!("features `cuda` and `rocm` are mutually exclusive; enable only one GPU backend");

// compute backend ที่ใช้งานจริงสำหรับ Whisper session ภายในเครื่อง
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputeBackend {
    // ใช้ CPU สำหรับ inference
    Cpu,
    // ใช้ NVIDIA CUDA สำหรับ inference
    Cuda,
    // ใช้ AMD ROCm/HIP สำหรับ inference
    Rocm,
}

impl ComputeBackend {
    // label ที่อ่านเข้าใจง่ายสำหรับหน้า Settings และส่วนแสดงสถานะ
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Cuda => "CUDA",
            Self::Rocm => "ROCm / HIP",
        }
    }

    // ระบุว่า Whisper context ต้องร้องขอให้ประมวลผลด้วย GPU หรือไม่
    pub const fn uses_gpu(self) -> bool {
        !matches!(self, Self::Cpu)
    }
}

// คืน compute backend ประสิทธิภาพสูงสุดที่ถูก compile ไว้ใน binary นี้
pub const fn compiled_compute_backend() -> ComputeBackend {
    #[cfg(all(feature = "cuda", not(feature = "rocm")))]
    {
        ComputeBackend::Cuda
    }
    #[cfg(all(feature = "rocm", not(feature = "cuda")))]
    {
        ComputeBackend::Rocm
    }
    #[cfg(not(any(feature = "cuda", feature = "rocm")))]
    {
        ComputeBackend::Cpu
    }
    #[cfg(all(feature = "cuda", feature = "rocm"))]
    {
        ComputeBackend::Cpu
    }
}

// ตรวจ GPU device ครั้งเดียวเพราะ hardware ไม่เปลี่ยนระหว่าง application session ปกติ
pub fn gpu_backend_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(detect_gpu_device)
}

// คืน backend ที่ใช้งานได้จริงโดยเรียง GPU ก่อน CPU
pub fn available_compute_requests() -> &'static [ComputeRequest] {
    if !gpu_backend_available() {
        return &[ComputeRequest::Cpu];
    }

    #[cfg(all(feature = "cuda", not(feature = "rocm")))]
    {
        &[ComputeRequest::Cuda, ComputeRequest::Cpu]
    }
    #[cfg(all(feature = "rocm", not(feature = "cuda")))]
    {
        &[ComputeRequest::Rocm, ComputeRequest::Cpu]
    }
    #[cfg(not(any(feature = "cuda", feature = "rocm")))]
    {
        &[ComputeRequest::Cpu]
    }
    #[cfg(all(feature = "cuda", feature = "rocm"))]
    {
        &[]
    }
}

// เลือก GPU ก่อนและ fallback เป็น CPU เมื่อไม่มี GPU ที่ใช้งานได้
pub fn preferred_compute_request() -> ComputeRequest {
    available_compute_requests()
        .first()
        .copied()
        .unwrap_or(ComputeRequest::Cpu)
}

// จับคู่ runtime request กับ compile capability และ GPU device ของ binary นี้
pub fn resolve_compute_backend(request: ComputeRequest) -> Result<ComputeBackend, String> {
    match request {
        ComputeRequest::Cpu => Ok(ComputeBackend::Cpu),
        ComputeRequest::Cuda if !cfg!(feature = "cuda") => Err(
            "CUDA was requested, but this binary was built without the `cuda` feature".to_owned(),
        ),
        ComputeRequest::Cuda if !gpu_backend_available() => {
            Err("CUDA was requested, but no usable NVIDIA GPU was detected".to_owned())
        }
        ComputeRequest::Cuda => Ok(ComputeBackend::Cuda),
        ComputeRequest::Rocm if !cfg!(feature = "rocm") => Err(
            "ROCm was requested, but this binary was built without the `rocm` feature".to_owned(),
        ),
        ComputeRequest::Rocm if !gpu_backend_available() => {
            Err("ROCm was requested, but no usable AMD GPU was detected".to_owned())
        }
        ComputeRequest::Rocm => Ok(ComputeBackend::Rocm),
    }
}

fn detect_gpu_device() -> bool {
    #[cfg(all(feature = "cuda", not(feature = "rocm")))]
    {
        return std::path::Path::new("/dev/nvidiactl").exists()
            && std::path::Path::new("/dev/nvidia0").exists();
    }

    #[cfg(all(feature = "rocm", not(feature = "cuda")))]
    {
        return std::path::Path::new("/dev/kfd").exists() && has_amd_render_node();
    }

    #[cfg(not(any(feature = "cuda", feature = "rocm")))]
    {
        false
    }

    #[cfg(all(feature = "cuda", feature = "rocm"))]
    {
        false
    }
}

#[cfg(all(feature = "rocm", not(feature = "cuda")))]
fn has_amd_render_node() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return false;
    };

    entries.filter_map(Result::ok).any(|entry| {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            return false;
        };
        if !file_name.starts_with("renderD") {
            return false;
        }
        std::fs::read_to_string(entry.path().join("device/vendor"))
            .is_ok_and(|vendor| vendor.trim().eq_ignore_ascii_case("0x1002"))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ComputeBackend, available_compute_requests, compiled_compute_backend,
        preferred_compute_request, resolve_compute_backend,
    };
    use crate::config::ComputeRequest;

    #[test]
    fn display_labels_cover_supported_backends() {
        assert_eq!(ComputeBackend::Cpu.display_name(), "CPU");
        assert_eq!(ComputeBackend::Cuda.display_name(), "CUDA");
        assert_eq!(ComputeBackend::Rocm.display_name(), "ROCm / HIP");
    }

    #[test]
    fn cpu_is_always_selectable() {
        assert_eq!(
            resolve_compute_backend(ComputeRequest::Cpu),
            Ok(ComputeBackend::Cpu)
        );
        assert!(!ComputeBackend::Cpu.uses_gpu());
        assert!(available_compute_requests().contains(&ComputeRequest::Cpu));
    }

    #[test]
    fn preferred_request_is_the_first_available_backend() {
        assert_eq!(preferred_compute_request(), available_compute_requests()[0]);
    }

    #[test]
    fn compiled_backend_matches_enabled_feature() {
        #[cfg(all(feature = "cuda", not(feature = "rocm")))]
        assert_eq!(compiled_compute_backend(), ComputeBackend::Cuda);
        #[cfg(all(feature = "rocm", not(feature = "cuda")))]
        assert_eq!(compiled_compute_backend(), ComputeBackend::Rocm);
        #[cfg(not(any(feature = "cuda", feature = "rocm")))]
        assert_eq!(compiled_compute_backend(), ComputeBackend::Cpu);
    }

    #[cfg(not(feature = "cuda"))]
    #[test]
    fn unavailable_cuda_request_has_a_clear_error() {
        let error = resolve_compute_backend(ComputeRequest::Cuda)
            .expect_err("CUDA should require the cuda build feature");

        assert!(error.contains("without the `cuda` feature"));
    }

    #[cfg(not(feature = "rocm"))]
    #[test]
    fn unavailable_rocm_request_has_a_clear_error() {
        let error = resolve_compute_backend(ComputeRequest::Rocm)
            .expect_err("ROCm should require the rocm build feature");

        assert!(error.contains("without the `rocm` feature"));
    }
}
