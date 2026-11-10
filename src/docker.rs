use crate::{LinuxDistro, Platforms, workspace_root};
use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use xshell::Cmd;
use xshell::{Shell, cmd};

const CONTAINER_ROOT: &str = "/src";

#[derive(serde::Deserialize)]
struct DockerBuildMetadata {
    #[serde(rename = "containerimage.config.digest")]
    digest: String,
}

pub fn container_release_dir(version: &str, metadata: &DockerImage) -> PathBuf {
    PathBuf::from(CONTAINER_ROOT)
        .join("artifacts")
        .join(version)
        .join(metadata.get_output_dir())
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

    fn get_docker_platform(platform: &Platforms) -> &'static str {
        match platform {
            Platforms::Amd64 => "linux/amd64",
            Platforms::Aarch64 => "linux/arm64/v8",
        }
    }

    fn run_as<'a>(
        &self,
        sh: &'a Shell,
        commands: impl IntoIterator<Item = &'a Cmd<'a>>,
        user: u32,
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
        let docker_platform = Self::get_docker_platform(&self.platform);
        let user = user.to_string();
        let cmd = cmd!(
            sh,
            "docker run --init --user {user}:{user} --platform {docker_platform} -w {CONTAINER_ROOT} -v {root}:{CONTAINER_ROOT} -v {script_file_path}:/command --rm {digest} sh /command"
        );
        cmd.run()?;
        Ok(())
    }

    pub fn run_as_user<'a>(
        &self,
        sh: &'a Shell,
        commands: impl IntoIterator<Item = &'a Cmd<'a>>,
    ) -> Result<()> {
        self.run_as(sh, commands, 1000)
    }

    pub fn run_as_root<'a>(
        &self,
        sh: &'a Shell,
        commands: impl IntoIterator<Item = &'a Cmd<'a>>,
    ) -> Result<()> {
        self.run_as(sh, commands, 0)
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
        let docker_platform = Self::get_docker_platform(platform);
        let cmd = cmd!(
            sh,
            "docker build --metadata-file {metadata_path} --platform {docker_platform} {tag...} -f {dockerfile} {root}"
        );
        cmd.run()?;
        serde_json::from_reader::<_, DockerBuildMetadata>(std::fs::File::open(metadata_path)?)
            .map_err(|e| anyhow::anyhow!("Failed to parse Docker metadata: {e}"))
    }

    pub fn shell_as_root(&self) -> Result<()> {
        self.shell_as(0)
    }

    pub fn shell_as_user(&self) -> Result<()> {
        self.shell_as(1000)
    }

    fn shell_as(&self, user: u32) -> Result<()> {
        let sh = Shell::new()?;
        let digest = &self.get_digest();
        let root = workspace_root();
        let docker_platform = Self::get_docker_platform(&self.platform);
        let user = user.to_string();
        let cmd = cmd!(
            sh,
            "docker run --user {user}:{user} --platform {docker_platform} -w {CONTAINER_ROOT} -v {root}:{CONTAINER_ROOT} --rm -it {digest} /bin/bash"
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
