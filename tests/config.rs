//! Exercise real startup in an isolated directory, never the repository's .env.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn dotenv_is_local_environment_wins_and_errors_are_redacted() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("saic-config-{}-{nonce}", std::process::id());
    let dir = Directory(std::env::temp_dir().join(name));
    fs::create_dir(&dir.0).unwrap();
    fs::write(dir.0.join(".env"), "SAIC_USERNAME=synthetic@example.invalid\nSAIC_PASSWORD=synthetic-private-password\nHTTP_PORT=invalid-private-port\nSAIC_BASE_URL=http://private-url.invalid/\n").unwrap();
    let run = |path: &std::path::Path, env: &[(&str, &str)]| {
        let output = Command::new(env!("CARGO_BIN_EXE_saic-rust-notify"))
            .current_dir(path)
            .env_clear()
            .envs(env.iter().copied())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(!error.contains("private"));
        assert!(!error.contains("synthetic@example.invalid"));
        assert!(output.stdout.is_empty());
        error
    };
    assert!(run(&dir.0, &[]).contains("HTTP_PORT"));
    assert!(run(&dir.0, &[("HTTP_PORT", "8080")]).contains("SAIC_BASE_URL"));
    assert!(run(&dir.0, &[("HTTP_PORT", "8080"), ("SAIC_PASSWORD", "")]).contains("SAIC_PASSWORD"));
    let child = dir.0.join("child");
    fs::create_dir(&child).unwrap();
    assert!(run(&child, &[]).contains("SAIC_USERNAME is required"));
}
