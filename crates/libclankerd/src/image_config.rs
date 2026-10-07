//! The OCI image config (`config.Entrypoint`, `Cmd`, `Env`, `User`,
//! `WorkingDir`) and how a machine's configuration is merged over it, with
//! Docker's rules for `--entrypoint`, trailing arguments, `-e`, `-u` and `-w`.

use serde::{Deserialize, Serialize};

use crate::config::MachineConfig;
use crate::error::{Error, Result};

/// The runtime defaults an image declares. Stored with each machine so its
/// root disk can be rebuilt from another image later without losing what the
/// developer overrode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageConfig {
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    /// `KEY=value` entries.
    pub env: Vec<String>,
    pub user: String,
    pub working_dir: String,
    /// The image's `StopSignal`; empty when it declares none.
    pub stop_signal: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct RawConfig {
    entrypoint: Option<Vec<String>>,
    cmd: Option<Vec<String>>,
    env: Option<Vec<String>>,
    user: Option<String>,
    working_dir: Option<String>,
    stop_signal: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawImage {
    config: Option<RawConfig>,
}

impl ImageConfig {
    /// Reads the `config` object of an OCI image config JSON document.
    /// Absent or `null` fields are empty, as Docker treats them.
    pub fn parse(json: &str) -> Result<Self> {
        let raw: RawImage = serde_json::from_str(json)
            .map_err(|e| Error::system(format!("reading the image config: {e}")))?;
        let c = raw.config.unwrap_or_default();
        Ok(ImageConfig {
            entrypoint: c.entrypoint.unwrap_or_default(),
            cmd: c.cmd.unwrap_or_default(),
            env: c.env.unwrap_or_default(),
            user: c.user.unwrap_or_default(),
            working_dir: c.working_dir.unwrap_or_default(),
            stop_signal: c.stop_signal.unwrap_or_default(),
        })
    }

    /// What actually runs: `requested` (the developer's overrides) over the
    /// image defaults.
    ///
    /// * An entrypoint override drops the image's CMD, as Docker does; an
    ///   override of `[""]` clears the entrypoint (`--entrypoint ""`).
    /// * Trailing arguments replace the image's CMD but keep its ENTRYPOINT.
    /// * Environment entries override image entries with the same key.
    /// * The stop signal is the developer's, else the image's `StopSignal`.
    /// * User and working directory replace the image's when given; the
    ///   working directory defaults to `/`.
    pub fn merge(&self, requested: &MachineConfig) -> Result<MachineConfig> {
        let (entrypoint, cmd) = if !requested.entrypoint.is_empty() {
            let entrypoint = if requested.entrypoint == [""] {
                Vec::new()
            } else {
                requested.entrypoint.clone()
            };
            (entrypoint, requested.cmd.clone())
        } else if !requested.cmd.is_empty() {
            (self.entrypoint.clone(), requested.cmd.clone())
        } else {
            (self.entrypoint.clone(), self.cmd.clone())
        };
        if entrypoint.is_empty() && cmd.is_empty() {
            return Err(Error::invalid_parameter(
                "no command specified: the image has no ENTRYPOINT or CMD",
            ));
        }
        let mut env = self.env.clone();
        for entry in &requested.env {
            let key = entry.split_once('=').map_or(entry.as_str(), |(k, _)| k);
            match env
                .iter_mut()
                .find(|e| e.split_once('=').map_or(e.as_str(), |(k, _)| k) == key)
            {
                Some(existing) => *existing = entry.clone(),
                None => env.push(entry.clone()),
            }
        }
        let pick = |own: &str, image: &str| if own.is_empty() { image } else { own }.to_owned();
        let working_dir = match pick(&requested.working_dir, &self.working_dir) {
            w if w.is_empty() => "/".to_owned(),
            w => w,
        };
        Ok(MachineConfig {
            image: requested.image.clone(),
            entrypoint,
            cmd,
            env,
            user: pick(&requested.user, &self.user),
            working_dir,
            tty: requested.tty,
            open_stdin: requested.open_stdin,
            stop_signal: pick(&requested.stop_signal, &self.stop_signal),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> ImageConfig {
        ImageConfig::parse(
            r#"{"architecture":"arm64","config":{
                "Entrypoint":["/entry.sh"],"Cmd":["serve","--fast"],
                "Env":["PATH=/bin","MODE=image"],"User":"agent","WorkingDir":"/srv"}}"#,
        )
        .unwrap()
    }

    fn req() -> MachineConfig {
        MachineConfig {
            image: "img".into(),
            ..Default::default()
        }
    }

    #[test]
    fn image_defaults_apply_when_nothing_is_overridden() {
        let m = image().merge(&req()).unwrap();
        assert_eq!(m.entrypoint, ["/entry.sh"]);
        assert_eq!(m.cmd, ["serve", "--fast"]);
        assert_eq!(m.env, ["PATH=/bin", "MODE=image"]);
        assert_eq!((m.user.as_str(), m.working_dir.as_str()), ("agent", "/srv"));
    }

    #[test]
    fn trailing_arguments_replace_cmd_and_keep_the_entrypoint() {
        let m = image()
            .merge(&MachineConfig {
                cmd: vec!["sleep".into(), "infinity".into()],
                ..req()
            })
            .unwrap();
        assert_eq!(m.entrypoint, ["/entry.sh"]);
        assert_eq!(m.cmd, ["sleep", "infinity"]);
    }

    #[test]
    fn an_entrypoint_override_drops_the_image_cmd() {
        let m = image()
            .merge(&MachineConfig {
                entrypoint: vec!["/bin/sh".into()],
                ..req()
            })
            .unwrap();
        assert_eq!(m.entrypoint, ["/bin/sh"]);
        assert!(m.cmd.is_empty());

        let m = image()
            .merge(&MachineConfig {
                entrypoint: vec!["/bin/sh".into()],
                cmd: vec!["-c".into(), "true".into()],
                ..req()
            })
            .unwrap();
        assert_eq!(m.cmd, ["-c", "true"]);
    }

    #[test]
    fn an_empty_entrypoint_override_clears_it() {
        let m = image()
            .merge(&MachineConfig {
                entrypoint: vec![String::new()],
                cmd: vec!["/bin/true".into()],
                ..req()
            })
            .unwrap();
        assert!(m.entrypoint.is_empty());
        assert_eq!(m.cmd, ["/bin/true"]);
    }

    #[test]
    fn environment_merges_by_key_with_overrides_winning() {
        let m = image()
            .merge(&MachineConfig {
                env: vec!["MODE=dev".into(), "EXTRA=1".into()],
                ..req()
            })
            .unwrap();
        assert_eq!(m.env, ["PATH=/bin", "MODE=dev", "EXTRA=1"]);
    }

    #[test]
    fn user_and_working_dir_overrides_win_and_workdir_defaults_to_root() {
        let m = image()
            .merge(&MachineConfig {
                user: "root".into(),
                working_dir: "/tmp".into(),
                ..req()
            })
            .unwrap();
        assert_eq!((m.user.as_str(), m.working_dir.as_str()), ("root", "/tmp"));

        let bare = ImageConfig::parse(r#"{"config":{"Cmd":["x"]}}"#).unwrap();
        let m = bare.merge(&req()).unwrap();
        assert_eq!((m.user.as_str(), m.working_dir.as_str()), ("", "/"));
    }

    #[test]
    fn null_and_missing_fields_are_empty_and_no_command_is_an_error() {
        let c = ImageConfig::parse(r#"{"config":{"Entrypoint":null,"Cmd":null}}"#).unwrap();
        assert_eq!(c, ImageConfig::default());
        assert!(ImageConfig::parse("{}").is_ok());
        let err = c.merge(&req()).unwrap_err();
        assert_eq!(err.kind(), crate::ErrorKind::InvalidParameter);
    }
}
