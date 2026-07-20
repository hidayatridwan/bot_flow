use anyhow::Context;
use std::path::Path;
use tokio::process::Command;

/// The sidecar refused the file, and its exit code says why. See `sidecar/parser.py` for the
/// contract; both sides must change together.
///
/// This is a *typed* error rather than a formatted string because [`crate::failure::classify`]
/// downcasts to it to decide what the tenant is told. Collapsing it back into `bail!("parser
/// failed: {stderr}")` — which is what it used to be — makes every parser outcome look like a
/// system fault, and the tenant is told to wait for a file only they can fix.
#[derive(Debug)]
pub struct SidecarExit {
    /// `None` when the process was killed by a signal and never returned a code.
    pub code: Option<i32>,
    pub stderr: String,
}

/// Unsupported extension. The tenant should upload a different format.
pub const EXIT_UNSUPPORTED_TYPE: i32 = 3;
/// Right type, unreadable content: encrypted, truncated or malformed. Re-uploading may fix it.
pub const EXIT_UNREADABLE: i32 = 4;

impl std::fmt::Display for SidecarExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code {
            Some(c) => write!(f, "parser exited {c}: {}", self.stderr),
            None => write!(f, "parser killed by signal: {}", self.stderr),
        }
    }
}

impl std::error::Error for SidecarExit {}

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
        return Err(SidecarExit {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        }
        .into());
    }

    String::from_utf8(output.stdout).context("parser output was not valid UTF-8")
}
