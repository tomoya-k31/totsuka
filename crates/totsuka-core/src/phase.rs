use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Design,
    ImplVerify,
}

impl Phase {
    pub fn as_snake(&self) -> &'static str {
        match self {
            Phase::Design => "design",
            Phase::ImplVerify => "impl_verify",
        }
    }
    /// branch 命名用の短縮形 (spec §11.14)
    pub fn as_short(&self) -> &'static str {
        match self {
            Phase::Design => "design",
            Phase::ImplVerify => "implv",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snake_serde() {
        assert_eq!(
            serde_json::to_string(&Phase::ImplVerify).unwrap(),
            "\"impl_verify\""
        );
        let p: Phase = serde_json::from_str("\"design\"").unwrap();
        assert_eq!(p, Phase::Design);
    }
    #[test]
    fn short_form_for_branch() {
        assert_eq!(Phase::ImplVerify.as_short(), "implv");
        assert_eq!(Phase::Design.as_short(), "design");
    }
}
