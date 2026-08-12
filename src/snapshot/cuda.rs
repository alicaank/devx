use std::path::{Path, PathBuf};

use super::{CudaInfo, command};

pub fn scan(cwd: &Path) -> CudaInfo {
    let cuda_home = std::env::var_os("CUDA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CUDA_PATH").map(PathBuf::from));
    let nvcc_path = command::find_executable("nvcc").or_else(|| {
        cuda_home
            .as_ref()
            .map(|home| home.join("bin/nvcc"))
            .filter(|p| p.is_file())
    });
    let nvcc_version = nvcc_path
        .as_ref()
        .and_then(|nvcc| command::output(nvcc, &["--version"], cwd))
        .and_then(|text| parse_release(&text));
    let smi =
        command::find_executable("nvidia-smi").and_then(|path| command::output(&path, &[], cwd));
    let driver_version = smi.as_deref().and_then(parse_driver_version);
    let driver_cuda = smi.as_deref().and_then(parse_driver_cuda);
    CudaInfo {
        cuda_home,
        nvcc_path,
        nvcc_version,
        driver_version,
        driver_cuda,
    }
}

fn parse_release(text: &str) -> Option<String> {
    let tail = text.split("release ").nth(1)?;
    Some(tail.split([',', ' ']).next()?.trim().to_owned())
}

fn parse_driver_cuda(text: &str) -> Option<String> {
    let tail = text.split("CUDA Version: ").nth(1)?;
    Some(tail.split_whitespace().next()?.to_owned())
}

fn parse_driver_version(text: &str) -> Option<String> {
    let tail = text.split("Driver Version: ").nth(1)?;
    Some(tail.split_whitespace().next()?.to_owned())
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_nvcc_release() {
        assert_eq!(
            super::parse_release("Cuda compilation tools, release 12.4, V12.4.99").as_deref(),
            Some("12.4")
        );
    }

    #[test]
    fn parses_driver_capability() {
        assert_eq!(
            super::parse_driver_cuda("Driver Version: 570.0  CUDA Version: 12.8"),
            Some("12.8".into())
        );
    }

    #[test]
    fn parses_driver_version() {
        assert_eq!(
            super::parse_driver_version(
                "NVIDIA-SMI 570.124.06  Driver Version: 570.124.06  CUDA Version: 12.8"
            ),
            Some("570.124.06".into())
        );
    }
}
