use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::registry::ORDER;
use std::io::{Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub async fn run(paths: &Paths, bin: &str, lines: u32, follow: bool) -> Result<(), TotsukactlError> {
    let path = log_path(paths, bin)?;
    if !path.exists() {
        return Err(TotsukactlError::Internal(format!("log file {path:?} not found")));
    }
    let text = std::fs::read_to_string(&path)?;
    print!("{}", tail_lines(&text, lines));
    if follow {
        follow_file(&path).await?;
    }
    Ok(())
}

fn log_path(paths: &Paths, bin: &str) -> Result<PathBuf, TotsukactlError> {
    if bin == "supervisor" {
        return Ok(paths.supervisor_log());
    }
    if !ORDER.contains(&bin) {
        return Err(TotsukactlError::UnknownChild(bin.into()));
    }
    Ok(paths.child_log(bin))
}

pub fn tail_lines(text: &str, n: u32) -> String {
    let n = n as usize;
    let mut lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines.drain(..start);
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

async fn follow_file(path: &std::path::Path) -> Result<(), TotsukactlError> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::End(0))?;
    let mut async_file = tokio::fs::File::from_std(file);
    let mut buf = vec![0u8; 4096];
    loop {
        let n = async_file.read(&mut buf).await?;
        if n == 0 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        use std::io::Write;
        std::io::stdout().write_all(&buf[..n])?;
        std::io::stdout().flush()?;
    }
}
