use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// Python 3.12.12 from python-build-standalone
// https://github.com/astral-sh/python-build-standalone/releases
const PYTHON_VERSION: &str = "3.12.12";
const PYTHON_BUILD_STANDALONE_VERSION: &str = "20251010";

fn main() {
    tauri_build::build();

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    // Download Python to src-tauri/python-runtime so Tauri can bundle it
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let python_dir = manifest_dir.join("python-runtime");

    // Check if Python actually exists (not just the directory)
    let python_binary_exists = if cfg!(windows) {
        python_dir.join("python").join("python.exe").exists()
    } else {
        python_dir.join("python").join("bin").join("python3").exists()
    };

    if !python_binary_exists {
        println!("cargo:warning=Downloading bundled Python runtime for {}-{}...", target_os, target_arch);

        if let Err(e) = download_and_extract_python(&target_os, &target_arch, &python_dir) {
            println!("cargo:warning=Failed to download Python runtime: {}", e);
            println!("cargo:warning=Build will continue but embedded Python may not be available");
        } else {
            println!("cargo:warning=Python runtime downloaded successfully to {}", python_dir.display());
        }
    } else {
        println!("cargo:warning=Using existing Python runtime at {}", python_dir.display());
    }

    // Tell cargo to rerun if this build script changes
    println!("cargo:rerun-if-changed=build.rs");
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
    let bytes = response.bytes()?;

    // Create destination directory
    fs::create_dir_all(dest_dir)?;

    // Extract based on archive type
    if is_tarball {
        extract_tarball(&bytes, dest_dir)?;
    } else {
        extract_zip(&bytes, dest_dir)?;
    }

    Ok(())
}

fn get_python_url(target_os: &str, target_arch: &str) -> Result<(String, bool), Box<dyn std::error::Error>> {
    let base_url = format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/{}/",
        PYTHON_BUILD_STANDALONE_VERSION
    );

    let (filename, is_tarball) = match (target_os, target_arch) {
        ("macos", "x86_64") => (
            format!("cpython-{}+{}-x86_64-apple-darwin-install_only.tar.gz", PYTHON_VERSION, PYTHON_BUILD_STANDALONE_VERSION),
            true
        ),
        ("macos", "aarch64") => (
            format!("cpython-{}+{}-aarch64-apple-darwin-install_only.tar.gz", PYTHON_VERSION, PYTHON_BUILD_STANDALONE_VERSION),
            true
        ),
        ("linux", "x86_64") => (
            format!("cpython-{}+{}-x86_64-unknown-linux-gnu-install_only.tar.gz", PYTHON_VERSION, PYTHON_BUILD_STANDALONE_VERSION),
            true
        ),
        ("linux", "aarch64") => (
            format!("cpython-{}+{}-aarch64-unknown-linux-gnu-install_only.tar.gz", PYTHON_VERSION, PYTHON_BUILD_STANDALONE_VERSION),
            true
        ),
        ("windows", "x86_64") => (
            format!("cpython-{}+{}-x86_64-pc-windows-msvc-shared-install_only.tar.gz", PYTHON_VERSION, PYTHON_BUILD_STANDALONE_VERSION),
            true
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

fn extract_zip(data: &[u8], dest_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Cursor;
    use zip::ZipArchive;

    let reader = Cursor::new(data);
    let mut archive = ZipArchive::new(reader)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = dest_dir.join(file.mangled_name());

        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut outfile = fs::File::create(&outpath)?;
            std::io::copy(&mut file, &mut outfile)?;
        }

        // Set permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                fs::set_permissions(&outpath, fs::Permissions::from_mode(mode))?;
            }
        }
    }

    Ok(())
}
