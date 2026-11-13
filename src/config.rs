use std::{env, time::Duration};

// Deliberately no Debug: this holds broker credentials and an account identifier.
#[derive(Clone)]
pub struct Config {
    pub http_host: String,
    pub http_port: u16,
    pub api_token: Option<String>,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub client_id: String,
    pub account: Option<String>,
    pub stale_after: Duration,
    pub stale_check: Duration,
    pub log_level: tracing::Level,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::load(|name| match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
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
        let host = read("MQTT_HOST", Some("localhost"))?;
        let port = positive(&read("MQTT_PORT", Some("1883"))?, "MQTT_PORT", 65535)? as u16;
        let username = read("MQTT_USERNAME", None)?;
        let password = read("MQTT_PASSWORD", None)?;
        let client_id = read("MQTT_CLIENT_ID", Some("saic-telemetry-consumer"))?;
        let stale_after = Duration::from_secs(positive(
            &read("STALE_AFTER_SECONDS", Some("600"))?,
            "STALE_AFTER_SECONDS",
            u32::MAX as u64,
        )?);
        let stale_check = Duration::from_secs(positive(
            &read("STALE_CHECK_SECONDS", Some("30"))?,
            "STALE_CHECK_SECONDS",
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
        let account = get("MQTT_ACCOUNT")?.filter(|s| !s.is_empty());
        if account.as_ref().is_some_and(|s| !valid_segment(s)) {
            return Err("MQTT_ACCOUNT must be one nonempty MQTT topic segment".into());
        }
        if username.len() > 65535 || password.len() > 65535 || client_id.len() > 65535 {
            return Err("MQTT credentials and client ID must fit MQTT string limits".into());
        }
        Ok(Self {
            http_host,
            http_port,
            api_token,
            host,
            port,
            username,
            password,
            client_id,
            account,
            stale_after,
            stale_check,
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
    !s.is_empty()
        && !s
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '+' | '#'))
}
