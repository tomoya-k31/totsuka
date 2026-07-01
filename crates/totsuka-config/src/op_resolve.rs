use crate::expand::ExpandError;
use std::process::Command;

/// Resolve one `op://vault/item/field` reference by invoking `op read`.
/// `bin` is the binary to invoke — a parameter (not hardcoded `"op"`) so
/// tests can point it at a fake script instead of mutating `PATH`.
pub fn resolve_with(bin: &str, uri: &str) -> Result<String, ExpandError> {
    let output = Command::new(bin)
        .args(["read", uri])
        .output()
        .map_err(|e| ExpandError::OpExec(uri.to_string(), e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(ExpandError::OpFailed(uri.to_string(), stderr));
    }

    let mut s =
        String::from_utf8(output.stdout).map_err(|_| ExpandError::OpNonUtf8(uri.to_string()))?;
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    Ok(s)
}

/// Production entry point — always invokes the real `op` CLI on `PATH`.
pub fn resolve(uri: &str) -> Result<String, ExpandError> {
    resolve_with("op", uri)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn resolve_success_strips_trailing_newline() {
        let dir = TempDir::new().unwrap();
        let script = write_script(&dir.path(), "fake-op", "#!/bin/sh\necho resolved-secret\n");
        assert_eq!(
            resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap(),
            "resolved-secret"
        );
    }

    #[test]
    fn resolve_success_no_trailing_newline_unchanged() {
        let dir = TempDir::new().unwrap();
        let script = write_script(
            &dir.path(),
            "fake-op",
            "#!/bin/sh\nprintf 'no-newline-value'\n",
        );
        assert_eq!(
            resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap(),
            "no-newline-value"
        );
    }

    #[test]
    fn resolve_success_strips_trailing_crlf() {
        let dir = TempDir::new().unwrap();
        let script = write_script(&dir.path(), "fake-op", "#!/bin/sh\nprintf 'value\\r\\n'\n");
        assert_eq!(
            resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap(),
            "value"
        );
    }

    #[test]
    fn resolve_passes_read_and_uri_as_args() {
        let dir = TempDir::new().unwrap();
        let script = write_script(
            &dir.path(),
            "fake-op",
            "#!/bin/sh\nif [ \"$1\" = read ] && [ \"$2\" = \"op://Vault/Item/field\" ]; then echo ok; else echo wrong-args; exit 1; fi\n",
        );
        assert_eq!(
            resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap(),
            "ok"
        );
    }

    #[test]
    fn resolve_nonzero_exit_is_op_failed() {
        let dir = TempDir::new().unwrap();
        let script = write_script(
            &dir.path(),
            "fake-op",
            "#!/bin/sh\necho 'not signed in' >&2\nexit 1\n",
        );
        match resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap_err() {
            ExpandError::OpFailed(uri, stderr) => {
                assert_eq!(uri, "op://Vault/Item/field");
                assert!(stderr.contains("not signed in"));
            }
            e => panic!("expected OpFailed, got {:?}", e),
        }
    }

    #[test]
    fn resolve_non_utf8_output_is_op_non_utf8() {
        let dir = TempDir::new().unwrap();
        let script = write_script(&dir.path(), "fake-op", "#!/bin/sh\nprintf '\\xff\\xfe'\n");
        match resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap_err() {
            ExpandError::OpNonUtf8(uri) => assert_eq!(uri, "op://Vault/Item/field"),
            e => panic!("expected OpNonUtf8, got {:?}", e),
        }
    }

    #[test]
    fn resolve_missing_binary_is_op_exec() {
        match resolve_with(
            "/nonexistent/path/to/binary-that-does-not-exist",
            "op://Vault/Item/field",
        )
        .unwrap_err()
        {
            ExpandError::OpExec(uri, _) => assert_eq!(uri, "op://Vault/Item/field"),
            e => panic!("expected OpExec, got {:?}", e),
        }
    }
}
