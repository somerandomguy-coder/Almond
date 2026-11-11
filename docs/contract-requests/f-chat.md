# Contract requests: feature `chat` ("Ask Almond")

**From:** ws/f-chat. **Status:** open, nothing blocked (each has a local workaround).

## 1. `ChatSource::person_id` (almond-types)

**What:** an optional link from a source chip to a person.

```rust
pub struct ChatSource {
    // ... existing fields
    /// `people::Person::id`, for `ChatSourceKind::Person` sources.
    #[serde(default)]
    pub person_id: Option<Id>,
}
```

**Why:** person sources (Person scope, or a person named in a Home question) can't open the
People feature's person page; the chip is inert today.
**Compat:** additive (`serde(default)`); regenerate `apps/desktop/src/gen/ChatSource.ts`.

## 2. API types for chat params/results (almond-types)

**What:** move `AskParams` / `AskResult` / `ChatDelta` (today in `almond_core::chat`) to
`almond_types::chat` with `#[derive(TS)]`, and add `notifications::CHAT_DELTA = "chat.delta"`
next to the other notification names.
**Why:** the UI mirrors them by hand in `apps/desktop/src/api/types.ts` (`ChatAskParams`,
`ChatDelta`).
**Compat:** additive.

## 3. Search index: recipe outputs and user notes (library / almond-store)

**What:** `almond_store::fts::artifact_docs` indexes `recipe_output` (`markdown`), and the notes
docs' plain text is indexed (kind `notes`).
**Why:** `store.search` (and `search.query`) can't find text that only appears in a recipe output
or the user's own notes. Chat works around it by scanning the notes and recipe outputs of the 30
most recent meetings for the question's keywords (`chat/context.rs`, `search_sources`).
**Compat:** additive; existing databases need a reindex of those artifacts/docs.

## 4. Optional: an "any keyword" search mode (almond-store)

**What:** `Store::search_any(tokens, limit)` (OR of prefix tokens, bm25-ranked).
**Why:** `search` ANDs every token, which rarely matches a natural-language question. Chat runs one
query per keyword plus the AND query and merges the scores itself (up to 7 queries per question;
fine at today's sizes).
**Compat:** additive.
