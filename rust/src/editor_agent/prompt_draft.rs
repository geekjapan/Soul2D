use eframe::egui::TextBuffer;

pub(super) struct PromptDraft {
    text: String,
    instance: Option<String>,
    revision: u64,
}

impl PromptDraft {
    pub(super) fn new() -> Self {
        Self { text: String::new(), instance: super::random_token().ok(), revision: 0 }
    }

    pub(super) fn identity(&self) -> Option<String> {
        self.instance.as_ref().filter(|_| self.revision != u64::MAX)
            .map(|instance| format!("{instance}:{}", self.revision))
    }

    pub(super) fn replace(&mut self, text: String) -> bool {
        if self.text == text { return false; }
        self.text = text;
        self.advance();
        true
    }

    fn advance(&mut self) { self.revision = self.revision.saturating_add(1); }

    #[cfg(test)]
    pub(super) fn version_for_test(&mut self, instance: Option<String>, revision: u64) {
        self.instance = instance;
        self.revision = revision;
    }
}

impl std::ops::Deref for PromptDraft {
    type Target = str;
    fn deref(&self) -> &str { &self.text }
}

impl TextBuffer for PromptDraft {
    fn is_mutable(&self) -> bool { true }
    fn as_str(&self) -> &str { &self.text }
    fn type_id(&self) -> std::any::TypeId { std::any::TypeId::of::<Self>() }

    fn insert_text(&mut self, text: &str, char_index: usize) -> usize {
        let inserted = TextBuffer::insert_text(&mut self.text, text, char_index);
        if inserted != 0 { self.advance(); }
        inserted
    }

    fn delete_char_range(&mut self, range: std::ops::Range<usize>) {
        let before = self.text.len();
        TextBuffer::delete_char_range(&mut self.text, range);
        if self.text.len() != before { self.advance(); }
    }

    fn clear(&mut self) { self.replace(String::new()); }
    fn replace_with(&mut self, text: &str) { self.replace(text.to_owned()); }
    fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        if !text.is_empty() { self.advance(); }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_agent_draft_buffer_tracks_unicode_defaults_noop_and_aba() {
        let mut draft = PromptDraft::new();
        assert_eq!(draft.type_id(), std::any::TypeId::of::<PromptDraft>());
        let initial = draft.identity();
        assert_eq!(draft.insert_text("é👁️", 0), 3);
        draft.insert_text("界", 1);
        assert_eq!(draft.as_str(), "é界👁️");
        draft.delete_char_range(1..2);
        assert_eq!(draft.as_str(), "é👁️");
        let before = draft.identity();
        draft.replace_with("é👁️");
        assert_eq!(draft.identity(), before);
        draft.replace_with("other"); draft.replace_with("é👁️");
        assert_ne!(draft.identity(), before);
        assert_eq!(draft.take(), "é👁️");
        let cleared = draft.identity();
        draft.clear(); draft.replace_with(""); assert_eq!(draft.take(), "");
        assert_eq!(draft.identity(), cleared);
        assert_ne!(draft.identity(), initial);
    }

    #[test]
    fn coding_agent_draft_instances_failure_and_exhaustion_preserve_native_edits() {
        let mut first = PromptDraft::new(); let second = PromptDraft::new();
        assert_ne!(first.identity(), second.identity());
        first.version_for_test(None, 0);
        first.replace_with("native despite identity failure");
        assert!(first.identity().is_none());
        first.version_for_test(Some("owned-instance".into()), u64::MAX - 1);
        assert!(first.identity().is_some());
        first.replace_with("last remote-compatible edit");
        assert!(first.identity().is_none());
        first.insert_text("native ", 0);
        assert!(first.as_str().starts_with("native ") && first.identity().is_none());
    }
}
