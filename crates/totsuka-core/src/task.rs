use serde::{Deserialize, Serialize};
use std::fmt;

/// ProjectV2Item.id (`PVTI_...`). totsuka は UUID を発行しない (spec §11.14)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(String);

impl TaskId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// branch 名 / ログ用の末尾 12 文字短縮形 (spec §11.14)
    pub fn short(&self) -> String {
        let s = &self.0;
        if s.len() <= 12 {
            s.clone()
        } else {
            s[s.len() - 12..].to_string()
        }
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for TaskId {
    fn from(s: String) -> Self {
        Self(s)
    }
}
impl From<&str> for TaskId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_takes_tail_12_chars() {
        let t = TaskId::new("PVTI_lAHOAjcRPs4AHvuRzgVabc123def456");
        assert_eq!(t.short(), "abc123def456");
        assert_eq!(t.short().len(), 12);
    }
    #[test]
    fn short_keeps_full_when_short() {
        assert_eq!(TaskId::new("short").short(), "short");
    }
    #[test]
    fn serde_transparent() {
        let t = TaskId::new("PVTI_x");
        assert_eq!(serde_json::to_string(&t).unwrap(), "\"PVTI_x\"");
    }
}
