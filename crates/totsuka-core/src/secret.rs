use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(inner: T) -> Self {
        Self(inner)
    }
    /// 内側を露出する。outbound HTTP / DB 接続文字列構築時のみ使用
    pub fn expose(&self) -> &T {
        &self.0
    }
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl<T> From<T> for Secret<T> {
    fn from(v: T) -> Self {
        Self::new(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_leak() {
        let s: Secret<String> = "supersecret".to_string().into();
        let d = format!("{:?}", s);
        assert!(!d.contains("supersecret"));
        assert_eq!(d, "Secret(***)");
    }

    #[test]
    fn display_does_not_leak() {
        let s: Secret<String> = "tk_abcdef".to_string().into();
        assert_eq!(format!("{}", s), "***");
    }

    #[test]
    fn expose_returns_inner() {
        let s: Secret<String> = "abc".to_string().into();
        assert_eq!(s.expose(), "abc");
    }

    #[test]
    fn deserialize_from_plain_string() {
        let s: Secret<String> = serde_json::from_str("\"v\"").unwrap();
        assert_eq!(s.expose(), "v");
    }
}
