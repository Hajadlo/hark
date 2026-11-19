use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub const MAX_RESTORE_DELAY_MS: u64 = 1000;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shortcut {
    pub key: String,
    pub mode: String,
}
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub url: String,
    pub token: String,
    pub device: String,
    pub shortcuts: Vec<Shortcut>,
    pub tap_ms: u64,
    pub restore_delay_ms: u64,
    pub input_device: Option<String>,
    pub sounds: bool,
    pub insert: String,
    pub overlay: bool,
    pub overlay_output: Option<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            url: "https://gpu-host.your-tailnet.ts.net:8766".into(),
            token: String::new(),
            device: "desktop".into(),
            shortcuts: vec![
                Shortcut {
                    key: "Ctrl+Space".into(),
                    mode: "clean".into(),
                },
                Shortcut {
                    key: "Ctrl+Shift+Space".into(),
                    mode: "raw".into(),
                },
            ],
            tap_ms: 300,
            restore_delay_ms: 60,
            input_device: None,
            sounds: true,
            insert: "paste".into(),
            overlay: true,
            overlay_output: None,
        }
    }
}
pub fn default_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("hark/config.toml")
}
impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        // Do not include TOML parse errors: they can quote the token's source line.
        let mut c: Self = toml::from_str(text)
            .map_err(|_| anyhow::anyhow!("invalid config TOML (check keys and types)"))?;
        c.url = c
            .url
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .trim_end_matches('/')
            .into();
        let url = reqwest::Url::parse(&c.url).context("invalid server URL")?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "URL must be an HTTP(S) base URL without credentials, query or fragment"
        );
        ensure!(
            c.token.len() >= 32 && !c.token.chars().any(char::is_control),
            "set a valid token of at least 32 characters"
        );
        ensure!(
            valid_name(&c.device),
            "device must be 1–32 lowercase letters, digits or hyphens"
        );
        ensure!(c.tap_ms > 0 && c.tap_ms <= 2000, "tap_ms must be 1–2000");
        ensure!(
            c.restore_delay_ms <= MAX_RESTORE_DELAY_MS,
            "restore_delay_ms must be 0–1000"
        );
        ensure!(
            matches!(c.insert.as_str(), "paste" | "print"),
            "insert must be paste or print"
        );
        ensure!(!c.shortcuts.is_empty(), "at least one shortcut is required");
        let mut keys = std::collections::HashSet::new();
        for s in &c.shortcuts {
            ensure!(valid_name(&s.mode), "invalid mode name");
            ensure!(
                keys.insert(crate::hotkeys::qt_key(&s.key)?),
                "duplicate shortcut"
            );
        }
        Ok(c)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let meta = fs::symlink_metadata(path).context("read config metadata")?;
        ensure!(meta.file_type().is_file(), "config must be a regular file");
        if meta.permissions().mode() & 0o777 != 0o600 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .context("secure config permissions")?;
            eprintln!("warning: config permissions fixed to 600");
        }
        Self::parse(&fs::read_to_string(path).context("read config")?)
    }
    pub fn redact(&self, text: &str) -> String {
        text.replace(&self.token, "[redacted]")
    }
}
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlay_defaults_and_override() {
        let c = Config::parse(&config()).unwrap();
        assert!(c.overlay);
        assert!(c.overlay_output.is_none());
        let c = Config::parse(&(config() + "overlay = false\noverlay_output = 'DP-4'\n")).unwrap();
        assert!(!c.overlay);
        assert_eq!(c.overlay_output.as_deref(), Some("DP-4"));
    }
    #[test]
    fn clipboard_restore_delay_defaults_and_override() {
        assert_eq!(Config::parse(&config()).unwrap().restore_delay_ms, 60);
        assert_eq!(
            Config::parse(&(config() + "restore_delay_ms = 250\n"))
                .unwrap()
                .restore_delay_ms,
            250
        );
        assert!(Config::parse(&(config() + "restore_delay_ms = -1\n")).is_err());
        for delay in [0, 1000] {
            assert_eq!(
                Config::parse(&(config() + &format!("restore_delay_ms = {delay}\n")))
                    .unwrap()
                    .restore_delay_ms,
                delay
            );
        }
        let error = Config::parse(&(config() + "restore_delay_ms = 1001\n"))
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("restore_delay_ms must be 0–1000")
        );
    }
    fn config() -> String {
        format!(
            "token = '{}'\nurl = 'https://example.test/v1/'\n",
            "t".repeat(32)
        )
    }
    #[test]
    fn defaults_normalization_validation() {
        let c = Config::parse(&config()).unwrap();
        assert_eq!(c.url, "https://example.test");
        assert_eq!(c.shortcuts[1].mode, "raw");
        assert_eq!(c.tap_ms, 300);
        assert!(Config::parse("token='secret'").is_err());
        assert!(Config::parse(&(config() + "insert='oops'")).is_err());
    }
    #[test]
    fn permissions_fixed_and_symlinks_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config");
        fs::write(&p, config()).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        Config::load(&p).unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        assert!(Config::load(&link).is_err());
    }
    #[test]
    fn parse_error_does_not_echo_secrets() {
        let token = "unique-secret-token-value-12345678";
        let err = Config::parse(&format!("token = '{token}' garbage"))
            .err()
            .unwrap();
        assert!(!err.to_string().contains(token));
    }
}
