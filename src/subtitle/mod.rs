//! Partial/final transcript reconciliation and subtitle presentation state.

use crate::stt::TranscriptUpdate;

const DEFAULT_PRESENTATION_WORDS: usize = 32;

#[derive(Debug)]
pub struct TranscriptReconciler {
    finalized: Vec<String>,
    provisional: Vec<String>,
    active_segment_id: Option<u64>,
    last_final_segment_id: Option<u64>,
    stable_word_count: usize,
    presentation: String,
    max_presentation_words: usize,
}

impl Default for TranscriptReconciler {
    fn default() -> Self {
        Self::new(DEFAULT_PRESENTATION_WORDS)
    }
}

impl TranscriptReconciler {
    pub fn new(max_presentation_words: usize) -> Self {
        Self {
            finalized: Vec::new(),
            provisional: Vec::new(),
            active_segment_id: None,
            last_final_segment_id: None,
            stable_word_count: 0,
            presentation: String::new(),
            max_presentation_words: max_presentation_words.max(1),
        }
    }

    pub fn apply(&mut self, update: &TranscriptUpdate) -> bool {
        let before = self.presentation.clone();
        match update {
            TranscriptUpdate::Partial { segment_id, text } => {
                let incoming = words(text);
                if self.active_segment_id == Some(*segment_id) {
                    self.stable_word_count = common_prefix_len(&self.provisional, &incoming);
                } else {
                    self.active_segment_id = Some(*segment_id);
                    self.stable_word_count = 0;
                }
                self.provisional = incoming;
            }
            TranscriptUpdate::Final { segment_id, text } => {
                if self.last_final_segment_id != Some(*segment_id) {
                    let incoming = words(text);
                    append_without_overlap(&mut self.finalized, &incoming);
                    trim_front(&mut self.finalized, self.max_presentation_words);
                    self.last_final_segment_id = Some(*segment_id);
                }
                if self.active_segment_id == Some(*segment_id) {
                    self.provisional.clear();
                    self.active_segment_id = None;
                    self.stable_word_count = 0;
                }
            }
        }
        self.refresh_presentation();
        self.presentation != before
    }

    pub fn presentation_text(&self) -> &str {
        &self.presentation
    }

    pub fn current_text(&self) -> &str {
        self.presentation_text()
    }

    pub const fn stable_word_count(&self) -> usize {
        self.stable_word_count
    }

    pub fn clear(&mut self) {
        self.finalized.clear();
        self.provisional.clear();
        self.active_segment_id = None;
        self.last_final_segment_id = None;
        self.stable_word_count = 0;
        self.presentation.clear();
    }

    fn refresh_presentation(&mut self) {
        let mut combined = self.finalized.clone();
        append_without_overlap(&mut combined, &self.provisional);
        trim_front(&mut combined, self.max_presentation_words);
        self.presentation = combined.join(" ");
    }
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn append_without_overlap(base: &mut Vec<String>, incoming: &[String]) {
    let overlap = longest_word_overlap(base, incoming);
    base.extend(incoming[overlap..].iter().cloned());
}

fn longest_word_overlap(base: &[String], incoming: &[String]) -> usize {
    let maximum = base.len().min(incoming.len());
    (1..=maximum)
        .rev()
        .find(|&length| {
            base[base.len() - length..]
                .iter()
                .zip(&incoming[..length])
                .all(|(left, right)| normalized_word(left) == normalized_word(right))
        })
        .unwrap_or(0)
}

fn common_prefix_len(left: &[String], right: &[String]) -> usize {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| normalized_word(left) == normalized_word(right))
        .count()
}

fn normalized_word(word: &str) -> String {
    word.trim_matches(|character: char| !character.is_alphanumeric())
        .to_lowercase()
}

fn trim_front(words: &mut Vec<String>, maximum: usize) {
    if words.len() > maximum {
        words.drain(..words.len() - maximum);
    }
}

#[cfg(test)]
mod tests {
    use super::TranscriptReconciler;
    use crate::stt::TranscriptUpdate;

    #[test]
    fn evolving_partial_replaces_the_same_segment() {
        let mut reconciler = TranscriptReconciler::default();

        reconciler.apply(&TranscriptUpdate::Partial {
            segment_id: 1,
            text: "I think we should".to_owned(),
        });
        reconciler.apply(&TranscriptUpdate::Partial {
            segment_id: 1,
            text: "I think we should go".to_owned(),
        });

        assert_eq!(reconciler.presentation_text(), "I think we should go");
        assert_eq!(reconciler.stable_word_count(), 4);
    }

    #[test]
    fn final_and_new_partial_use_the_longest_word_overlap() {
        let mut reconciler = TranscriptReconciler::default();

        reconciler.apply(&TranscriptUpdate::Final {
            segment_id: 1,
            text: "we should go now".to_owned(),
        });
        reconciler.apply(&TranscriptUpdate::Partial {
            segment_id: 2,
            text: "go now before dark".to_owned(),
        });

        assert_eq!(
            reconciler.presentation_text(),
            "we should go now before dark"
        );
    }

    #[test]
    fn duplicate_final_segment_is_ignored() {
        let mut reconciler = TranscriptReconciler::default();
        let update = TranscriptUpdate::Final {
            segment_id: 7,
            text: "Hello there.".to_owned(),
        };

        reconciler.apply(&update);
        reconciler.apply(&update);

        assert_eq!(reconciler.presentation_text(), "Hello there.");
    }
}
