use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// Python 3.12.12 from python-build-standalone
// https://github.com/astral-sh/python-build-standalone/releases
const PYTHON_VERSION: &str = "3.12.12";
const PYTHON_BUILD_STANDALONE_VERSION: &str = "20251010";

// ffmpeg static binaries from eugeneware/ffmpeg-static
// https://github.com/eugeneware/ffmpeg-static/releases
const FFMPEG_VERSION: &str = "b6.1.1";

fn main() {
    tauri_build::build();

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();

    // Use actual system architecture for Python, not Rust target
    // Python packages need to match the system, not the Rust build
    let system_arch = get_system_arch();
    let cargo_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    println!(
        "cargo:warning=System arch from uname: {:?}, Cargo arch: {}",
        system_arch, cargo_arch
    );
    let target_arch = system_arch.unwrap_or(cargo_arch);

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    // Download Python to src-tauri/python-runtime so Tauri can bundle it
    let python_dir = manifest_dir.join("python-runtime");

    // Check if Python actually exists (not just the directory)
    let python_binary_exists = if cfg!(windows) {
        python_dir.join("python").join("python.exe").exists()
    } else {
        python_dir
            .join("python")
            .join("bin")
            .join("python3")
            .exists()
    };

    if !python_binary_exists {
        println!(
            "cargo:warning=Downloading bundled Python runtime for {}-{}...",
            target_os, target_arch
        );

        if let Err(e) = download_and_extract_python(&target_os, &target_arch, &python_dir) {
            println!("cargo:warning=Failed to download Python runtime: {}", e);
            println!("cargo:warning=Build will continue but embedded Python may not be available");
        } else {
            println!(
                "cargo:warning=Python runtime downloaded successfully to {}",
                python_dir.display()
            );
        }
    } else {
        println!(
            "cargo:warning=Using existing Python runtime at {}",
            python_dir.display()
        );
    }

    // Download ffmpeg to src-tauri/ffmpeg-runtime so Tauri can bundle it
    let ffmpeg_dir = manifest_dir.join("ffmpeg-runtime");
    let ffmpeg_binary = if cfg!(windows) {
        ffmpeg_dir.join("ffmpeg.exe")
    } else {
        ffmpeg_dir.join("ffmpeg")
    };

    if !ffmpeg_binary.exists() {
        println!(
            "cargo:warning=Downloading bundled ffmpeg for {}-{}...",
            target_os, target_arch
        );

        if let Err(e) = download_ffmpeg(&target_os, &target_arch, &ffmpeg_dir) {
            println!("cargo:warning=Failed to download ffmpeg: {}", e);
            println!("cargo:warning=Build will continue but bundled ffmpeg may not be available");
        } else {
            println!(
                "cargo:warning=ffmpeg downloaded successfully to {}",
                ffmpeg_dir.display()
            );
        }
    } else {
        println!(
            "cargo:warning=Using existing ffmpeg at {}",
            ffmpeg_binary.display()
        );
    }

    // Tell cargo to rerun if this build script changes
    println!("cargo:rerun-if-changed=build.rs");
    // Also rerun if the Python runtime binary changes or is deleted
    if cfg!(windows) {
        println!("cargo:rerun-if-changed=python-runtime/python/python.exe");
        println!("cargo:rerun-if-changed=ffmpeg-runtime/ffmpeg.exe");
    } else {
        println!("cargo:rerun-if-changed=python-runtime/python/bin/python3");
        println!("cargo:rerun-if-changed=ffmpeg-runtime/ffmpeg");
    }
}

fn download_ffmpeg(
    target_os: &str,
    target_arch: &str,
    dest_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = get_ffmpeg_url(target_os, target_arch)?;

    println!("cargo:warning=Downloading ffmpeg from: {}", url);

    let response = reqwest::blocking::get(&url)?;
    if !response.status().is_success() {
        return Err(format!(
            "Failed to download ffmpeg: HTTP {} from {}",
            response.status(),
            url
        )
        .into());
    }
    let compressed = response.bytes()?;

    fs::create_dir_all(dest_dir)?;

    // Decompress gzip -> single binary
    use flate2::read::GzDecoder;
    use std::io::Read;

    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut binary = Vec::new();
    decoder.read_to_end(&mut binary)?;

    let binary_name = if target_os == "windows" {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let binary_path = dest_dir.join(binary_name);
    fs::write(&binary_path, &binary)?;

    // Set executable permission on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o755))?;
    }

    Ok(())
}

fn get_ffmpeg_url(
    target_os: &str,
    target_arch: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let base_url = format!(
        "https://github.com/eugeneware/ffmpeg-static/releases/download/{}/",
        FFMPEG_VERSION
    );

    let filename = match (target_os, target_arch) {
        ("macos", "x86_64") => "ffmpeg-darwin-x64.gz",
        ("macos", "aarch64") => "ffmpeg-darwin-arm64.gz",
        ("linux", "x86_64") => "ffmpeg-linux-x64.gz",
        ("linux", "aarch64") => "ffmpeg-linux-arm64.gz",
        ("windows", "x86_64") => "ffmpeg-win32-x64.gz",
        (os, arch) => {
            return Err(format!("Unsupported platform for ffmpeg: {}-{}", os, arch).into())
        }
    };

    Ok(format!("{}{}", base_url, filename))
}

fn download_and_extract_python(
    target_os: &str,
    target_arch: &str,
    dest_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (url, is_tarball) = get_python_url(target_os, target_arch)?;

    println!("cargo:warning=Downloading from: {}", url);

    // Download the archive
    let response = reqwest::blocking::get(&url)?;
    if !response.status().is_success() {
        return Err(format!(
            "Failed to download Python runtime: HTTP {} from {}",
            response.status(),
            url
        )
        .into());
    }
    let bytes = response.bytes()?;

    // Create destination directory
    fs::create_dir_all(dest_dir)?;

    // Extract based on archive type
    if is_tarball {
        // all platform URLs are tarballs
        extract_tarball(&bytes, dest_dir)?;
    }

    Ok(())
}

fn get_system_arch() -> Option<String> {
    // Get actual system architecture for Python compatibility
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        // Use sysctl to get the actual hardware architecture
        // This works even when running under Rosetta
        let output = Command::new("sysctl")
            .args(["-n", "hw.optional.arm64"])
            .output()
            .ok()?;
        let is_arm64 = String::from_utf8(output.stdout)
            .ok()?
            .trim()
            .parse::<u32>()
            .ok()?
            == 1;

        Some(if is_arm64 {
            "aarch64".to_string()
        } else {
            "x86_64".to_string()
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn get_python_url(
    target_os: &str,
    target_arch: &str,
) -> Result<(String, bool), Box<dyn std::error::Error>> {
    let base_url = format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/{}/",
        PYTHON_BUILD_STANDALONE_VERSION
    );

    let (filename, is_tarball) = match (target_os, target_arch) {
        ("macos", "x86_64") => (
            format!(
                "cpython-{}+{}-x86_64-apple-darwin-install_only.tar.gz",
                PYTHON_VERSION, PYTHON_BUILD_STANDALONE_VERSION
            ),
            true,
        ),
        ("macos", "aarch64") => (
            format!(
                "cpython-{}+{}-aarch64-apple-darwin-install_only.tar.gz",
                PYTHON_VERSION, PYTHON_BUILD_STANDALONE_VERSION
            ),
            true,
        ),
        ("linux", "x86_64") => (
            format!(
                "cpython-{}+{}-x86_64-unknown-linux-gnu-install_only.tar.gz",
                PYTHON_VERSION, PYTHON_BUILD_STANDALONE_VERSION
            ),
            true,
        ),
        ("linux", "aarch64") => (
            format!(
                "cpython-{}+{}-aarch64-unknown-linux-gnu-install_only.tar.gz",
                PYTHON_VERSION, PYTHON_BUILD_STANDALONE_VERSION
            ),
            true,
        ),
        ("windows", "x86_64") => (
            format!(
                "cpython-{}+{}-x86_64-pc-windows-msvc-shared-install_only.tar.gz",
                PYTHON_VERSION, PYTHON_BUILD_STANDALONE_VERSION
            ),
            true,
        ),
        (os, arch) => return Err(format!("Unsupported platform: {}-{}", os, arch).into()),
    };

    Ok((format!("{}{}", base_url, filename), is_tarball))
}

fn extract_tarball(data: &[u8], dest_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(data);
    let mut archive = Archive::new(decoder);
    archive.unpack(dest_dir)?;

    Ok(())
}
