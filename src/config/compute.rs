// compute backend request ที่ serialize และบันทึกลง application config

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};

// compute backend ที่ร้องขอสำหรับ Whisper session ภายในเครื่อง
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ComputeRequest {
    /// ใช้ CPU สำหรับ inference แม้ binary จะรองรับ GPU แล้วก็ตาม
    #[default]
    Cpu,
    /// ใช้ NVIDIA CUDA สำหรับ inference
    Cuda,
    /// ใช้ AMD ROCm/HIP สำหรับ inference
    Rocm,
}

impl ComputeRequest {
    /// คืนค่าคงที่สำหรับบันทึกในไฟล์ config
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::Rocm => "rocm",
        }
    }

    /// label ที่อ่านเข้าใจง่ายสำหรับหน้า Settings และส่วนแสดงสถานะ
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Cuda => "CUDA",
            Self::Rocm => "ROCm / HIP",
        }
    }
}

impl fmt::Display for ComputeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.display_name())
    }
}

impl<'de> Deserialize<'de> for ComputeRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "cpu" | "auto" => Ok(Self::Cpu),
            "cuda" => Ok(Self::Cuda),
            "rocm" => Ok(Self::Rocm),
            _ => Err(de::Error::unknown_variant(&value, &["cpu", "cuda", "rocm"])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ComputeRequest;

    #[test]
    fn cpu_is_the_default_request() {
        assert_eq!(ComputeRequest::default(), ComputeRequest::Cpu);
    }

    #[test]
    fn legacy_auto_deserializes_as_cpu() {
        let request: ComputeRequest = serde_json::from_str(r#""auto""#)
            .expect("legacy auto compute request should remain readable");

        assert_eq!(request, ComputeRequest::Cpu);
    }

    #[test]
    fn requests_serialize_to_stable_lowercase_values() {
        assert_eq!(
            serde_json::to_string(&ComputeRequest::Rocm).expect("request should serialize"),
            r#""rocm""#
        );
    }
}
