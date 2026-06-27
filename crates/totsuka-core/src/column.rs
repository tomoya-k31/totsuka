use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// spec §11.4: 8 カラムの正規化。serde は snake_case 文字列
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnId {
    Inbox,
    Ready,
    Design,
    DesignReview,
    ImplVerify,
    FinalReview,
    AwaitingRelease,
    Released,
}

impl ColumnId {
    pub const ALL: [ColumnId; 8] = [
        ColumnId::Inbox,
        ColumnId::Ready,
        ColumnId::Design,
        ColumnId::DesignReview,
        ColumnId::ImplVerify,
        ColumnId::FinalReview,
        ColumnId::AwaitingRelease,
        ColumnId::Released,
    ];
    pub fn as_snake(&self) -> &'static str {
        match self {
            ColumnId::Inbox => "inbox",
            ColumnId::Ready => "ready",
            ColumnId::Design => "design",
            ColumnId::DesignReview => "design_review",
            ColumnId::ImplVerify => "impl_verify",
            ColumnId::FinalReview => "final_review",
            ColumnId::AwaitingRelease => "awaiting_release",
            ColumnId::Released => "released",
        }
    }
}

/// 表示名 (GitHub Project の絵文字付き和文) ↔ ColumnId
#[derive(Debug, Clone)]
pub struct ColumnMap {
    display_to_id: HashMap<String, ColumnId>,
    id_to_display: HashMap<ColumnId, String>,
}

impl ColumnMap {
    /// 8 値が全て map に揃っているかチェックして構築。欠落・余剰はエラー
    pub fn try_new(displays: HashMap<ColumnId, String>) -> Result<Self, ColumnMapError> {
        for id in ColumnId::ALL {
            if !displays.contains_key(&id) {
                return Err(ColumnMapError::Missing(id));
            }
        }
        let mut display_to_id = HashMap::new();
        for (id, name) in &displays {
            if display_to_id.insert(name.clone(), *id).is_some() {
                return Err(ColumnMapError::DuplicateDisplay(name.clone()));
            }
        }
        Ok(Self {
            display_to_id,
            id_to_display: displays,
        })
    }

    pub fn resolve(&self, display: &str) -> Option<ColumnId> {
        self.display_to_id.get(display).copied()
    }
    pub fn display(&self, id: ColumnId) -> &str {
        self.id_to_display
            .get(&id)
            .expect("constructor ensures coverage")
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ColumnMapError {
    #[error("column display name missing for {0:?}")]
    Missing(ColumnId),
    #[error("duplicate display name: {0}")]
    DuplicateDisplay(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_map() -> HashMap<ColumnId, String> {
        let mut m = HashMap::new();
        m.insert(ColumnId::Inbox, "📥 Inbox".into());
        m.insert(ColumnId::Ready, "📋 Ready".into());
        m.insert(ColumnId::Design, "🤖 調査・設計".into());
        m.insert(ColumnId::DesignReview, "🚧 設計レビュー".into());
        m.insert(ColumnId::ImplVerify, "🤖 実装・受入検証".into());
        m.insert(ColumnId::FinalReview, "🚧 最終レビュー".into());
        m.insert(ColumnId::AwaitingRelease, "🚀 リリース待ち".into());
        m.insert(ColumnId::Released, "🏁 完了".into());
        m
    }

    #[test]
    fn snake_case_serde_roundtrip() {
        let s = serde_json::to_string(&ColumnId::ImplVerify).unwrap();
        assert_eq!(s, "\"impl_verify\"");
        let c: ColumnId = serde_json::from_str(&s).unwrap();
        assert_eq!(c, ColumnId::ImplVerify);
    }

    #[test]
    fn map_resolves_japanese_emoji_displays() {
        let m = ColumnMap::try_new(full_map()).unwrap();
        assert_eq!(m.resolve("🤖 調査・設計"), Some(ColumnId::Design));
        assert_eq!(m.display(ColumnId::Released), "🏁 完了");
    }

    #[test]
    fn missing_column_errors() {
        let mut partial = full_map();
        partial.remove(&ColumnId::Inbox);
        let err = ColumnMap::try_new(partial).unwrap_err();
        assert_eq!(err, ColumnMapError::Missing(ColumnId::Inbox));
    }

    #[test]
    fn unknown_display_returns_none() {
        let m = ColumnMap::try_new(full_map()).unwrap();
        assert_eq!(m.resolve("nope"), None);
    }
}
