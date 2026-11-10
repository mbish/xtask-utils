use anyhow::Result;
use std::fmt::{Debug, Display};
use std::path::PathBuf;
use strum::Display;
use xshell::{Shell, cmd};

pub mod aptly;
pub mod docker;

pub trait LinuxDistro: ToString + Display + Debug {
    fn get_nickname(&self) -> &'static str;
    fn get_aptly_repo_name(&self) -> String {
        let package = std::env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "unknown".to_string());
        let os = self.get_nickname();
        format!("{package}-{os}",)
    }
}

#[derive(Clone, Copy, Debug, Display)]
pub enum LinuxDistros {
    Ubuntu24_04,
    Debian12,
    Debian13,
}

impl LinuxDistro for LinuxDistros {
    fn get_nickname(&self) -> &'static str {
        match self {
            Self::Ubuntu24_04 => "noble",
            Self::Debian12 => "bookworm",
            Self::Debian13 => "trixie",
        }
    }
}

#[derive(Clone, Copy, Debug, Display, strum::EnumIter)]
pub enum Platforms {
    Amd64,
    Aarch64,
}
impl Platforms {
    pub fn as_folder(&self) -> &'static str {
        match self {
            Self::Amd64 => "linux_amd64",
            Self::Aarch64 => "linux_arm64",
        }
    }

    pub fn as_human_readable(&self) -> &'static str {
        match self {
            Self::Amd64 => "64-bit Intel/AMD",
            Self::Aarch64 => "64-bit ARM",
        }
    }

    pub fn from_host() -> Result<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Ok(Platforms::Amd64),
            "aarch64" => Ok(Platforms::Aarch64),
            p => anyhow::bail!("Unsupported platform {p}"),
        }
    }
}

pub fn workspace_root() -> PathBuf {
    let xtask_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"));
    xtask_dir
        .parent()
        .expect("Failed to find workspace root parent directory")
        .to_path_buf()
}

pub fn artifacts_dir() -> PathBuf {
    let root = workspace_root();
    root.join("artifacts")
}

pub fn require_package(package: &str) -> Result<()> {
    which::which(package).map_err(|_| {
        anyhow::anyhow!("Required package '{package}' is not installed. Please install it")
    })?;
    Ok(())
}

pub fn cargo_clean(sh: &Shell) -> Result<()> {
    sh.change_dir(workspace_root());
    cmd!(sh, "cargo clean").run()?;
    Ok(())
}
pub fn require_directory(path: &PathBuf) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)
            .map_err(|e| anyhow::anyhow!("Failed to create directory '{}': {e}", path.display()))?;
    }
    Ok(())
}

pub fn require_cargo_extension(extension: &str) -> Result<()> {
    let sh = Shell::new()?;
    let extensions = cmd!(sh, "cargo install --list").read()?;
    if !extensions.contains(extension) {
        anyhow::bail!("Required cargo extension '{extension}' is not installed. Please install it");
    }
    Ok(())
}

pub fn cargo_packager_deb(sh: &Shell, package: &str, target: &PathBuf) -> Result<()> {
    require_cargo_extension("cargo-packager")?;
    require_directory(target)?;
    cmd!(
        sh,
        "cargo packager --release -f deb -p {package} -o {target}"
    )
    .run()?;
    Ok(())
}

pub fn require_release_version(version: &str) -> Result<()> {
    if version.contains("dev") || version.contains("test") {
        anyhow::bail!("Release version is not set. This action must be done on a release branch");
    }
    Ok(())
}
