use std::{env, time::Duration};

// Deliberately no Debug: this holds account credentials.
#[derive(Clone)]
pub struct Config {
    pub notify: crate::notify::Config,
    pub http_host: String,
    pub http_port: u16,
    pub api_token: Option<String>,
    pub username: String,
    pub password: String,
    pub vin: Option<String>,
    pub api_base_url: Option<String>,
    pub stale_after: Duration,
    pub log_level: tracing::Level,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        // Read only the current directory; environment values take precedence.
        let file = match dotenvy::from_path_iter(".env") {
            Ok(iter) => iter
                .collect::<Result<std::collections::HashMap<_, _>, _>>()
                .map_err(|_| "Could not parse .env".to_owned())?,
            Err(dotenvy::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Default::default()
            }
            Err(_) => return Err("Could not read .env".into()),
        };
        Self::load(|name| match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(file.get(name).cloned()),
            Err(_) => Err(format!("{name} must be valid Unicode")),
        })
    }

    pub fn load(
        mut get: impl FnMut(&str) -> Result<Option<String>, String>,
    ) -> Result<Self, String> {
        let mut read = |name: &str, default: Option<&str>| -> Result<String, String> {
            let value = get(name)?
                .or_else(|| default.map(str::to_owned))
                .ok_or_else(|| format!("{name} is required"))?;
            if value.trim().is_empty() || value.contains('\0') {
                return Err(format!("{name} must be nonempty and contain no NUL"));
            }
            Ok(value)
        };
        let http_host = read("HTTP_HOST", Some("127.0.0.1"))?;
        let http_port = positive(&read("HTTP_PORT", Some("8080"))?, "HTTP_PORT", 65535)? as u16;
        let username = read("SAIC_USERNAME", None)?;
        let password = read("SAIC_PASSWORD", None)?;
        let stale_after = Duration::from_secs(positive(
            &read("STALE_AFTER_SECONDS", Some("600"))?,
            "STALE_AFTER_SECONDS",
            u32::MAX as u64,
        )?);
        let log_level = match read("LOG_LEVEL", Some("INFO"))?
            .to_ascii_uppercase()
            .as_str()
        {
            "ERROR" => tracing::Level::ERROR,
            "WARN" | "WARNING" => tracing::Level::WARN,
            "INFO" => tracing::Level::INFO,
            "DEBUG" => tracing::Level::DEBUG,
            "TRACE" => tracing::Level::TRACE,
            _ => return Err("LOG_LEVEL must be ERROR, WARN, WARNING, INFO, DEBUG or TRACE".into()),
        };
        let api_token = get("CLIMATE_API_TOKEN")?.filter(|s| !s.is_empty());
        let vin = get("SAIC_VIN")?.filter(|s| !s.is_empty());
        if vin.as_ref().is_some_and(|s| !valid_segment(s)) {
            return Err("SAIC_VIN must be a nonempty identifier".into());
        }
        let api_base_url = get("SAIC_BASE_URL")?.filter(|s| !s.is_empty());
        let notify = crate::notify::Config::load(&mut get)?;
        Ok(Self {
            notify,
            http_host,
            http_port,
            api_token,
            username,
            password,
            vin,
            api_base_url,
            stale_after,
            log_level,
        })
    }
}

fn positive(raw: &str, name: &str, max: u64) -> Result<u64, String> {
    raw.parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && *n <= max)
        .ok_or_else(|| format!("{name} must be an integer between 1 and {max}"))
}

pub(crate) fn valid_segment(s: &str) -> bool {
    !s.trim().is_empty()
        && !s
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '+' | '#'))
}
