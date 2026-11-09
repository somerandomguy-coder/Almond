use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// BCP-47 language tag ("vi", "en", "zh", "zh-Hans", ...). Kept as a string so new languages
/// never need a contract change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LangTag(pub String);

impl LangTag {
    pub fn vi() -> Self {
        LangTag("vi".into())
    }
    pub fn en() -> Self {
        LangTag("en".into())
    }
    pub fn zh() -> Self {
        LangTag("zh".into())
    }
    /// Primary subtag, lowercased: "zh-Hans" -> "zh".
    pub fn primary(&self) -> String {
        self.0
            .split(['-', '_'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    }
}
