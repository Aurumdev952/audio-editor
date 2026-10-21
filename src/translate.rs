//! Kinyarwanda -> English caption translation through OpenRouter (non-Claude models only).

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Command, Stdio};

pub const DEFAULT_MODEL: &str = "google/gemini-3.5-flash-lite";

pub struct Job {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

pub fn check_model(model: &str) -> Result<()> {
    let m = model.to_lowercase();
    if m.starts_with("anthropic/") || m.contains("claude") {
        bail!("Claude models must not be run through OpenRouter; pick another model");
    }
    Ok(())
}

fn api_key() -> Result<String> {
    if let Ok(k) = std::env::var("OPENROUTER_API_KEY") {
        return Ok(k);
    }
    let out = Command::new("keysvc").args(["get", "or-key"]).output().context("no OpenRouter key: set OPENROUTER_API_KEY")?;
    if !out.status.success() {
        bail!("no OpenRouter key: set OPENROUTER_API_KEY");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn translate(model: &str, job: &Job) -> Result<String> {
    check_model(model)?;
    let prompt = format!(
        "Translate one subtitle line of a Kinyarwanda conversation into natural, concise English. \
         The Kinyarwanda comes from speech recognition and may contain errors and English/French code-switching. \
         Reply with the English subtitle only.\n\nPrevious lines:\n{}\n\nLINE TO TRANSLATE:\n{}\n\nNext lines:\n{}",
        job.before.join("\n"),
        job.text,
        job.after.join("\n")
    );
    let body = serde_json::json!({
        "model": model,
        "temperature": 0.2,
        "messages": [{"role": "user", "content": prompt}],
    });
    // keep the key out of the process list: pass the auth header through a private curl config file
    let cfg = std::env::temp_dir().join(format!("audio-editor-{}-curl.cfg", std::process::id()));
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&cfg)?
        .write_all(format!("header = \"Authorization: Bearer {}\"\n", api_key()?).as_bytes())?;
    let mut child = Command::new("curl")
        .args(["-sS", "--max-time", "60", "-K"])
        .arg(&cfg)
        .args(["-H", "Content-Type: application/json", "--data-binary", "@-", "https://openrouter.ai/api/v1/chat/completions"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting curl")?;
    child.stdin.take().unwrap().write_all(body.to_string().as_bytes())?;
    let out = child.wait_with_output();
    let _ = std::fs::remove_file(&cfg);
    let out = out?;
    let resp: serde_json::Value = serde_json::from_slice(&out.stdout).with_context(|| String::from_utf8_lossy(&out.stderr).into_owned())?;
    match resp["choices"][0]["message"]["content"].as_str() {
        Some(t) => Ok(t.trim().trim_matches('"').to_string()),
        None => bail!("unexpected response: {}", resp.to_string().chars().take(300).collect::<String>()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_claude_models() {
        assert!(super::check_model("anthropic/claude-haiku-5.5").is_err());
        assert!(super::check_model("Claude-anything").is_err());
        assert!(super::check_model(super::DEFAULT_MODEL).is_ok());
    }
}
