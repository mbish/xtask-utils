use anyhow::Result;
use std::fmt::{Debug, Display};
use std::path::PathBuf;
use strum::Display;
use xshell::{Shell, cmd};

pub mod aptly;
pub mod docker;

pub trait LinuxDistro: ToString + Display + Debug {
    fn get_nickname(&self) -> &'static str;
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

pub fn as_distro_list<D: Into<LinuxDistros>, I: Iterator<Item = D>>(
    l: I,
) -> Vec<Box<dyn LinuxDistro>> {
    l.into_iter()
        .map(as_boxed_distro)
        .collect::<Vec<Box<dyn LinuxDistro>>>()
}

pub fn as_boxed_distro<D: Into<LinuxDistros>>(d: D) -> Box<dyn LinuxDistro> {
    Box::<LinuxDistros>::new(d.into()) as Box<dyn LinuxDistro>
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

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum PackageFormat {
    Deb,
    Appimage,
    Dmg,
    Msi,
}

pub fn make_file_name(
    format: PackageFormat,
    package: String,
    version: &str,
    platform: Platforms,
) -> Result<String> {
    let name = match format {
        PackageFormat::Deb | PackageFormat::Appimage => format!(
            "{}_{}_{}.{}",
            package,
            version,
            format.platform_as_package_suffix(platform),
            format.extension()
        ),
        PackageFormat::Msi => format!(
            "{}_{}_{}_{}.{}",
            package,
            version,
            format.platform_as_package_suffix(platform),
            "en-US",
            format.extension()
        ),
        PackageFormat::Dmg => {
            let root = workspace_root();
            let cargo_toml = root.join(package).join("Cargo.toml");
            let sh = Shell::new()?;
            require_package("tq")?;
            let metadata_name = {
                let name = cmd!(
                    sh,
                    "tq -r --file {cargo_toml} package.metadata.packager.product-name"
                )
                .read()?;
                name.replace(' ', ".")
            };
            format!(
                "{}_{}_{}.{}",
                metadata_name,
                version,
                format.platform_as_package_suffix(platform),
                format.extension()
            )
        }
    };
    Ok(name)
}

impl PackageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            PackageFormat::Deb => "deb",
            PackageFormat::Appimage => "AppImage",
            PackageFormat::Dmg => "dmg",
            PackageFormat::Msi => "msi",
        }
    }

    pub fn as_human_readable(self) -> &'static str {
        match self {
            PackageFormat::Deb => "(.deb) Debian Package",
            PackageFormat::Appimage => "(.AppImage) Linux AppImage",
            PackageFormat::Dmg => "(.dmg) macOS Disk Image",
            PackageFormat::Msi => "(.msi) Windows Installer",
        }
    }

    fn platform_as_package_suffix(self, platform: Platforms) -> &'static str {
        match (platform, self) {
            (Platforms::Amd64, PackageFormat::Deb) => "amd64",
            (Platforms::Amd64, PackageFormat::Appimage | PackageFormat::Dmg) => "x86_64",
            (Platforms::Amd64, PackageFormat::Msi) => "x64",
            (Platforms::Aarch64, PackageFormat::Deb | PackageFormat::Msi) => "arm64",
            (Platforms::Aarch64, PackageFormat::Appimage | PackageFormat::Dmg) => "aarch64",
        }
    }
}
