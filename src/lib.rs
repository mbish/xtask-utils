use anyhow::Result;
use std::fmt::Display;
use std::io::Write;
use std::path::PathBuf;
use xshell::Cmd;
use xshell::{Shell, cmd};

const CONTAINER_ROOT: &str = "/src";

pub trait Platform: Copy + Display {
    fn to_docker_platform(self) -> &'static str;
    fn as_folder(self) -> &'static str;
    fn as_human_readable(self) -> &'static str;
    fn from_host() -> Result<Self>
    where
        Self: Sized;
}

pub trait LinuxDistro: ToString + Sized + Display {
    fn get_nickname(self) -> &'static str;
    fn get_aptly_repo_name(self) -> String;
    fn run_release_tasks<P: Platform>(
        self,
        version: &str,
        metadata: &DockerMetadata<Self, P>,
    ) -> Result<()>;
}

#[derive(serde::Deserialize)]
struct DockerBuildMetadata {
    #[serde(rename = "containerimage.config.digest")]
    digest: String,
}

pub struct DockerMetadata<D: LinuxDistro, P: Platform> {
    build_metadata: DockerBuildMetadata,
    os: D,
    platform: P,
}

impl<D: LinuxDistro, P: Platform> DockerMetadata<D, P> {
    fn get_output_dir(&self) -> PathBuf {
        PathBuf::from(self.os.to_string()).join(self.platform.as_folder())
    }
    fn from_build_metadata(build_metadata: DockerBuildMetadata, os: D, platform: P) -> Self {
        DockerMetadata {
            build_metadata,
            os,
            platform,
        }
    }
}

pub fn workspace_root() -> PathBuf {
    let xtask_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    xtask_dir
        .parent()
        .expect("Failed to find workspace root parent directory")
        .to_path_buf()
}

pub fn artifacts_dir() -> PathBuf {
    let root = workspace_root();
    root.join("artifacts")
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

pub fn run_on_image<'a, D: LinuxDistro, P: Platform>(
    sh: &'a Shell,
    image: &DockerMetadata<D, P>,
    commands: impl IntoIterator<Item = &'a Cmd<'a>>,
) -> Result<()> {
    let digest = &image.build_metadata.digest;
    let root = workspace_root();
    let mut script_file = tempfile::Builder::new().append(true).tempfile()?;
    script_file.write_all(b"#!/bin/sh\nset -eu\n")?;
    script_file.write_all(b"trap 'kill -TERM $CHILD_PID 2>/dev/null; exit 1' INT TERM\n\n")?;
    script_file.write_all(
        commands
            .into_iter()
            .map(|cmd| wrap_as_background(sh, cmd))
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
    let docker_platform = image.platform.to_docker_platform();
    let cmd = cmd!(
        sh,
        "docker run --init --user 1000:1000 --platform {docker_platform} -w {CONTAINER_ROOT} -v {root}:{CONTAINER_ROOT} -v {script_file_path}:/command --rm {digest} sh /command"
    );
    cmd.run()?;
    Ok(())
}

pub fn require_package(package: &str) -> Result<()> {
    which::which(package).map_err(|_| {
        anyhow::anyhow!("Required package '{package}' is not installed. Please install it")
    })?;
    Ok(())
}

pub fn find_dockerfile<D: LinuxDistro>(os: &D) -> Option<PathBuf> {
    let root = workspace_root();
    let dockerfile_path = root
        .join("packaging")
        .join("docker-build")
        .join(os.to_string());
    if dockerfile_path.exists() {
        Some(dockerfile_path)
    } else {
        None
    }
}

fn build_docker_image<P: Platform>(
    platform: P,
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

pub fn build_docker_distro<D: LinuxDistro, P: Platform>(
    version: &str,
    platform: P,
    os: D,
) -> Result<DockerMetadata<D, P>> {
    let dockerfile = find_dockerfile(&os).ok_or_else(|| {
        anyhow::anyhow!("No Dockerfile found for platform {platform} and OS {os}")
    })?;
    let tag = format!(
        "autogain-packaging-{}-{}:{}",
        os.to_string().to_lowercase(),
        platform.as_folder(),
        version
    );
    let metadata = build_docker_image(platform, &dockerfile, Some(&tag))?;
    Ok(DockerMetadata::from_build_metadata(metadata, os, platform))
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

pub fn container_release_dir<D: LinuxDistro, P: Platform>(
    version: &str,
    metadata: &DockerMetadata<D, P>,
) -> PathBuf {
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
