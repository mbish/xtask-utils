use anyhow::Result;
use std::fmt::{Debug, Display};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use strum::Display;
use xshell::Cmd;
use xshell::{Shell, cmd};

const CONTAINER_ROOT: &str = "/src";

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
    pub fn to_docker_platform(&self) -> &'static str {
        match self {
            Self::Amd64 => "linux/amd64",
            Self::Aarch64 => "linux/arm64/v8",
        }
    }

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

#[derive(serde::Deserialize)]
struct DockerBuildMetadata {
    #[serde(rename = "containerimage.config.digest")]
    digest: String,
}

pub struct DockerImage {
    build_metadata: DockerBuildMetadata,
    os: String,
    output_folder: PathBuf,
    platform: Platforms,
}

impl DockerImage {
    fn get_output_dir(&self) -> PathBuf {
        PathBuf::from(self.os.to_string()).join(&self.output_folder)
    }
    fn from_build_metadata(
        build_metadata: DockerBuildMetadata,
        os: &impl LinuxDistro,
        platform: Platforms,
    ) -> Self {
        DockerImage {
            build_metadata,
            os: os.to_string(),
            output_folder: PathBuf::from(platform.as_folder()),
            platform,
        }
    }
    pub fn get_platform(&self) -> Platforms {
        self.platform
    }

    pub fn get_digest(&self) -> &str {
        &self.build_metadata.digest
    }
}

impl DockerImage {
    pub fn build_image(
        package: &str,
        version: &str,
        platform: Platforms,
        os: &impl LinuxDistro,
        dockerfile: &PathBuf,
    ) -> Result<Self> {
        let tag = format!(
            "{package}-packaging-{}-{}:{}",
            os.to_string().to_lowercase(),
            platform.as_folder(),
            version
        );
        let metadata = Self::build_docker_image(&platform, &dockerfile, Some(&tag))?;
        Ok(Self::from_build_metadata(metadata, os, platform))
    }

    fn wrap_as_background(sh: &Shell, cmd: &Cmd) -> String {
        let cmd_str = cmd.to_string();
        [
            cmd!(sh, "echo $ {cmd_str}").to_string(),
            cmd!(sh, "{cmd_str} &").to_string(),
            cmd!(sh, "CHILD_PID=$!").to_string(),
            cmd!(sh, "wait $CHILD_PID").to_string(),
        ]
        .join("\n")
        .clone()
            + "\n"
    }

    pub fn run_on_image<'a>(
        &self,
        sh: &'a Shell,
        commands: impl IntoIterator<Item = &'a Cmd<'a>>,
    ) -> Result<()> {
        let digest = &self.build_metadata.digest;
        let root = workspace_root();
        let mut script_file = tempfile::Builder::new().append(true).tempfile()?;
        script_file.write_all(b"#!/bin/sh\nset -eu\n")?;
        script_file.write_all(b"trap 'kill -TERM $CHILD_PID 2>/dev/null; exit 1' INT TERM\n\n")?;
        script_file.write_all(
            commands
                .into_iter()
                .map(|cmd| Self::wrap_as_background(sh, cmd))
                .collect::<Vec<_>>()
                .join("\n")
                .as_bytes(),
        )?;
        let script_file_path = script_file
            .path()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Failed to convert script file path to string"))?;
        println!("Running commands on image {digest} with script file {script_file_path}");
        println!(
            "Script contents:\n{}",
            std::fs::read_to_string(script_file_path)?
        );
        let docker_platform = self.get_platform().to_docker_platform();
        let cmd = cmd!(
            sh,
            "docker run --init --user 1000:1000 --platform {docker_platform} -w {CONTAINER_ROOT} -v {root}:{CONTAINER_ROOT} -v {script_file_path}:/command --rm {digest} sh /command"
        );
        cmd.run()?;
        Ok(())
    }

    fn build_docker_image(
        platform: &Platforms,
        dockerfile: &PathBuf,
        tag: Option<&str>,
    ) -> Result<DockerBuildMetadata> {
        let sh = Shell::new()?;
        let root = workspace_root();
        let metadata_file = tempfile::Builder::new().tempfile()?;
        let metadata_path = metadata_file
            .path()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Failed to convert metadata file path to string"))?;
        let tag = if let Some(tag) = tag {
            vec!["-t", tag]
        } else {
            vec![]
        };
        let docker_platform = platform.to_docker_platform();
        let cmd = cmd!(
            sh,
            "docker build --metadata-file {metadata_path} --platform {docker_platform} {tag...} -f {dockerfile} {root}"
        );
        cmd.run()?;
        serde_json::from_reader::<_, DockerBuildMetadata>(std::fs::File::open(metadata_path)?)
            .map_err(|e| anyhow::anyhow!("Failed to parse Docker metadata: {e}"))
    }

    pub fn shell_on_image(&self) -> Result<()> {
        let sh = Shell::new()?;
        let digest = &self.get_digest();
        let root = workspace_root();
        let docker_platform = self.get_platform().to_docker_platform();
        let cmd = cmd!(
            sh,
            "docker run --user 1000:1000 --platform {docker_platform} -w {CONTAINER_ROOT} -v {root}:{CONTAINER_ROOT} --rm -it {digest} /bin/bash"
        );
        let mut std_cmd: Command = cmd.into();
        std_cmd
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let mut child = std_cmd.spawn()?;
        child.wait()?;
        Ok(())
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

pub fn container_release_dir(version: &str, metadata: &DockerImage) -> PathBuf {
    PathBuf::from(CONTAINER_ROOT)
        .join("artifacts")
        .join(version)
        .join(metadata.get_output_dir())
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
