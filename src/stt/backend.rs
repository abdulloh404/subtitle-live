//! capability ของ compute backend ที่ compile รวมอยู่ใน binary ปัจจุบัน

use crate::config::ComputeRequest;

#[cfg(all(feature = "cuda", feature = "rocm"))]
compile_error!("features `cuda` and `rocm` are mutually exclusive; enable only one GPU backend");

/// compute backend ที่ใช้งานจริงสำหรับ Whisper session ภายในเครื่อง
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComputeBackend {
    /// ใช้ CPU สำหรับ inference
    Cpu,
    /// ใช้ NVIDIA CUDA สำหรับ inference
    Cuda,
    /// ใช้ AMD ROCm/HIP สำหรับ inference
    Rocm,
}

impl ComputeBackend {
    /// label ที่อ่านเข้าใจง่ายสำหรับหน้า Settings และส่วนแสดงสถานะ
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Cuda => "CUDA",
            Self::Rocm => "ROCm / HIP",
        }
    }

    /// ระบุว่า Whisper context ต้องร้องขอให้ประมวลผลด้วย GPU หรือไม่
    pub const fn uses_gpu(self) -> bool {
        !matches!(self, Self::Cpu)
    }
}

/// คืน compute backend ประสิทธิภาพสูงสุดที่ถูก compile ไว้ใน binary นี้
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

/// คืน runtime request ที่ binary นี้รองรับ
pub const fn available_compute_requests() -> &'static [ComputeRequest] {
    #[cfg(all(feature = "cuda", not(feature = "rocm")))]
    {
        &[ComputeRequest::Cpu, ComputeRequest::Cuda]
    }
    #[cfg(all(feature = "rocm", not(feature = "cuda")))]
    {
        &[ComputeRequest::Cpu, ComputeRequest::Rocm]
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

/// จับคู่ runtime request กับ capability ของ binary นี้
pub fn resolve_compute_backend(request: ComputeRequest) -> Result<ComputeBackend, String> {
    match request {
        ComputeRequest::Cpu => Ok(ComputeBackend::Cpu),
        ComputeRequest::Cuda if cfg!(feature = "cuda") => Ok(ComputeBackend::Cuda),
        ComputeRequest::Rocm if cfg!(feature = "rocm") => Ok(ComputeBackend::Rocm),
        ComputeRequest::Cuda => Err(
            "CUDA was requested, but this binary was built without the `cuda` feature".to_owned(),
        ),
        ComputeRequest::Rocm => Err(
            "ROCm was requested, but this binary was built without the `rocm` feature".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ComputeBackend, available_compute_requests, compiled_compute_backend,
        resolve_compute_backend,
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
