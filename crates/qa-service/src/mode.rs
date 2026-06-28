//! spec §8.4: default_mode = "auto" | "delegated".

use crate::error::QaError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerMode {
    Auto,
    Delegated,
}

impl AnswerMode {
    pub fn parse(s: &str) -> Result<Self, QaError> {
        match s {
            "auto" => Ok(Self::Auto),
            "delegated" => Ok(Self::Delegated),
            other => Err(QaError::Internal(format!("unknown default_mode: {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_known() {
        assert_eq!(AnswerMode::parse("auto").unwrap(), AnswerMode::Auto);
        assert_eq!(
            AnswerMode::parse("delegated").unwrap(),
            AnswerMode::Delegated
        );
    }
    #[test]
    fn rejects_unknown() {
        assert!(AnswerMode::parse("xyz").is_err());
    }
}
