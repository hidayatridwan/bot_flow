use anyhow::{bail, Context};
use std::path::Path;
use tokio::process::Command;

/// Extract plain text from raw file bytes by delegating to the Python sidecar.
/// We write the bytes to a temp file (named by document_id to avoid collisions),
/// hand the path to the sidecar, and capture its stdout.
pub async fn parse_to_text(
    bytes: &[u8],
    filename: &str,
    document_id: &str,
) -> anyhow::Result<String> {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let tmp = std::env::temp_dir().join(format!("{document_id}.{ext}"));

    tokio::fs::write(&tmp, bytes)
        .await
        .context("failed to write temp file for parser")?;

    let python = std::env::var("PARSER_PYTHON").unwrap_or_else(|_| "python3".into());
    let script = std::env::var("PARSER_SCRIPT").unwrap_or_else(|_| "sidecar/parser.py".into());

    let output = Command::new(&python)
        .arg(&script)
        .arg(&tmp)
        .output()
        .await
        .with_context(|| format!("failed to spawn parser: {python} {script}"))?;

    let _ = tokio::fs::remove_file(&tmp).await; // best-effort cleanup

    if !output.status.success() {
        bail!(
            "parser failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    String::from_utf8(output.stdout).context("parser output was not valid UTF-8")
}
