//! การรวมข้อความถอดเสียงแบบชั่วคราวและยืนยันแล้ว พร้อมสถานะที่ใช้แสดงเป็นคำบรรยาย

use crate::stt::{TranscriptUpdate, normalize_hypothesis_text};

// จำกัดประวัติที่แสดงเพื่อไม่ให้ข้อความสะสมเพิ่มหน่วยความจำอย่างไม่มีขอบเขต
const DEFAULT_PRESENTATION_WORDS: usize = 64;
// ความกว้างเป้าหมายต่อบรรทัดเมื่อผู้ใช้อนุญาตให้แสดงหลายบรรทัด
const CAPTION_LINE_CHARACTERS: usize = 42;
// โหมดบรรทัดเดียวเก็บข้อความล่าสุดภายในความกว้างของหน้าต่างคำบรรยาย
const SINGLE_LINE_CHARACTERS: usize = 128;

/// รวมสมมติฐานที่ Whisper แก้ซ้ำให้เป็นข้อความต่อเนื่องโดยปกป้องคำที่นิ่งแล้ว
#[derive(Debug)]
pub struct TranscriptReconciler {
    /// คำจากช่วงที่ยืนยันแล้วก่อนหน้า ซึ่งจะไม่ถูกแก้โดยผลชั่วคราวใหม่
    finalized: Vec<String>,
    /// คำนำหน้าของช่วงปัจจุบันที่ตรงกันหลายรอบแล้ว
    active_committed: Vec<String>,
    /// ส่วนท้ายล่าสุดที่ Whisper ยังสามารถแก้ได้
    provisional: Vec<String>,
    /// สมมติฐานรอบก่อน ใช้หาคำนำหน้าร่วมและช่วงที่เลื่อนซ้อนกัน
    previous_hypothesis: Vec<String>,
    /// รหัสช่วงผลชั่วคราวที่กำลังประมวลผล
    active_segment_id: Option<u64>,
    /// รหัสผลยืนยันล่าสุด ใช้ปฏิเสธเหตุการณ์ซ้ำหรือมาช้า
    last_final_segment_id: Option<u64>,
    /// จำนวนคำจากต้นสมมติฐานที่เคยยืนยันไว้แล้ว
    active_prefix_len: usize,
    /// จำนวนคำที่นิ่งในช่วงปัจจุบันสำหรับรายงานสถานะ UI
    stable_word_count: usize,
    /// ข้อความรวมที่ตัวทำงานหลักอ่านได้โดยไม่ต้องประกอบใหม่
    presentation: String,
    /// จำนวนคำสูงสุดในประวัติที่ใช้แสดง
    max_presentation_words: usize,
}

impl Default for TranscriptReconciler {
    fn default() -> Self {
        Self::new(DEFAULT_PRESENTATION_WORDS)
    }
}

impl TranscriptReconciler {
    /// สร้างตัวรวมข้อความโดยบังคับให้เก็บได้อย่างน้อยหนึ่งคำ
    pub fn new(max_presentation_words: usize) -> Self {
        Self {
            finalized: Vec::new(),
            active_committed: Vec::new(),
            provisional: Vec::new(),
            previous_hypothesis: Vec::new(),
            active_segment_id: None,
            last_final_segment_id: None,
            active_prefix_len: 0,
            stable_word_count: 0,
            presentation: String::new(),
            max_presentation_words: max_presentation_words.max(1),
        }
    }

    /// รวมเหตุการณ์หนึ่งรายการและคืนค่า `true` เมื่อข้อความสำหรับแสดงเปลี่ยนจริง
    pub fn apply(&mut self, update: &TranscriptUpdate) -> bool {
        let before = self.presentation.clone();
        match update {
            TranscriptUpdate::Partial { segment_id, text } => {
                let incoming = words(text);
                if incoming.is_empty() {
                    return false;
                }
                self.apply_partial(*segment_id, incoming);
            }
            TranscriptUpdate::Final { segment_id, text } => {
                let incoming = words(text);
                if incoming.is_empty() {
                    return false;
                }
                self.apply_final(*segment_id, incoming);
            }
        }
        self.refresh_presentation();
        self.presentation != before
    }

    /// คืนข้อความต่อเนื่องล่าสุดสำหรับจัดรูปแบบและแสดงบนหน้าต่างคำบรรยาย
    pub fn presentation_text(&self) -> &str {
        &self.presentation
    }

    /// ชื่อเรียกแบบย่อที่คงไว้ให้ผู้เรียกเดิมใช้ข้อความปัจจุบัน
    pub fn current_text(&self) -> &str {
        self.presentation_text()
    }

    /// คืนจำนวนคำที่ผ่านการยืนยันในช่วงชั่วคราวปัจจุบัน
    pub const fn stable_word_count(&self) -> usize {
        self.stable_word_count
    }

    /// ล้างทุกช่วงข้อความเมื่อหยุดกระบวนการหรือเปลี่ยนแหล่งเสียง
    pub fn clear(&mut self) {
        self.finalized.clear();
        self.active_committed.clear();
        self.provisional.clear();
        self.previous_hypothesis.clear();
        self.active_segment_id = None;
        self.last_final_segment_id = None;
        self.active_prefix_len = 0;
        self.stable_word_count = 0;
        self.presentation.clear();
    }

    /// รวมผลชั่วคราวโดยย้ายเฉพาะคำที่มีหลักฐานว่านิ่งแล้วไปยังส่วนที่ยืนยันภายใน
    fn apply_partial(&mut self, segment_id: u64, incoming: Vec<String>) {
        if self
            .last_final_segment_id
            .is_some_and(|finalized| segment_id <= finalized)
        {
            return;
        }

        if self.active_segment_id != Some(segment_id) {
            self.commit_active_without_final();
            self.active_segment_id = Some(segment_id);
            self.previous_hypothesis = incoming.clone();
            self.provisional = incoming;
            self.active_prefix_len = 0;
            self.stable_word_count = 0;
            return;
        }

        let common_prefix = common_prefix_len(&self.previous_hypothesis, &incoming);
        let shifted_overlap = longest_word_overlap(&self.previous_hypothesis, &incoming);

        if common_prefix == 0 && shifted_overlap == 0 {
            append_without_overlap(&mut self.active_committed, &self.previous_hypothesis);
            self.active_prefix_len = 0;
        } else if shifted_overlap > common_prefix
            && shifted_overlap < self.previous_hypothesis.len()
        {
            append_without_overlap(&mut self.active_committed, &self.previous_hypothesis);
            self.active_prefix_len = shifted_overlap;
        } else {
            if common_prefix > self.active_prefix_len {
                append_without_overlap(
                    &mut self.active_committed,
                    &incoming[self.active_prefix_len..common_prefix],
                );
            }
            self.active_prefix_len = self.active_prefix_len.max(common_prefix);
        }
        trim_front(&mut self.active_committed, self.max_presentation_words);

        self.provisional = incoming[self.active_prefix_len.min(incoming.len())..].to_vec();
        self.previous_hypothesis = incoming;
        self.stable_word_count = self.active_committed.len();
    }

    /// ปิดช่วงปัจจุบันและต่อผลยืนยันโดยหลีกเลี่ยงคำซ้ำบริเวณรอยต่อ
    fn apply_final(&mut self, segment_id: u64, incoming: Vec<String>) {
        if self
            .last_final_segment_id
            .is_some_and(|finalized| segment_id <= finalized)
        {
            return;
        }
        self.last_final_segment_id = Some(segment_id);

        if self.active_segment_id == Some(segment_id) {
            let mut segment = self.active_committed.clone();
            if incoming.is_empty() {
                segment.extend(self.provisional.iter().cloned());
            } else {
                let overlap = longest_word_overlap(&segment, &incoming);
                if overlap > 0 || segment.is_empty() {
                    segment.extend(incoming[overlap..].iter().cloned());
                } else if let Some(tail_start) = aligned_suffix_tail_start(&segment, &incoming) {
                    append_without_overlap(&mut segment, &incoming[tail_start..]);
                } else {
                    let protected_prefix = self.active_prefix_len.min(incoming.len());
                    append_without_overlap(&mut segment, &incoming[protected_prefix..]);
                }
            }
            self.finalized.extend(segment);
            trim_front(&mut self.finalized, self.max_presentation_words);
            self.clear_active();
        } else {
            self.finalized.extend(incoming);
            trim_front(&mut self.finalized, self.max_presentation_words);
        }
    }

    /// เก็บผลชั่วคราวเดิมเมื่อรหัสช่วงเปลี่ยนก่อนมีผลยืนยัน เพื่อไม่ให้คำหาย
    fn commit_active_without_final(&mut self) {
        if self.active_segment_id.is_none() {
            return;
        }

        let mut segment = self.active_committed.clone();
        segment.extend(self.provisional.iter().cloned());
        self.finalized.extend(segment);
        trim_front(&mut self.finalized, self.max_presentation_words);
        self.clear_active();
    }

    /// ล้างสถานะเฉพาะช่วงปัจจุบันหลังย้ายคำไปยังประวัติที่ยืนยันแล้ว
    fn clear_active(&mut self) {
        self.active_committed.clear();
        self.provisional.clear();
        self.previous_hypothesis.clear();
        self.active_segment_id = None;
        self.active_prefix_len = 0;
        self.stable_word_count = 0;
    }

    /// ประกอบคำที่ยืนยันแล้ว คำที่นิ่ง และคำที่ยังแก้ได้ใหม่ตามลำดับเวลา
    fn refresh_presentation(&mut self) {
        let mut combined = self.finalized.clone();
        let mut active = self.active_committed.clone();
        active.extend(self.provisional.iter().cloned());
        combined.extend(active);
        trim_front(&mut combined, self.max_presentation_words);
        self.presentation = combined.join(" ");
    }
}

/// ทำความสะอาดเครื่องหมายพิเศษแล้วแยกข้อความเป็นคำสำหรับการรวมผล
fn words(text: &str) -> Vec<String> {
    normalize_hypothesis_text(text)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// จัดข้อความล่าสุดเป็นจำนวนบรรทัดที่ผู้ใช้กำหนด
pub(crate) fn format_live_caption(text: &str, max_lines: u32) -> String {
    let words = words(text);
    format_caption_words(&words, max_lines)
}

/// เลือกการจัดแบบบรรทัดเดียวหรือหลายบรรทัดจากรายการคำที่ทำความสะอาดแล้ว
fn format_caption_words(words: &[String], max_lines: u32) -> String {
    if words.is_empty() {
        return String::new();
    }

    let keep = max_lines.max(1) as usize;
    if keep == 1 {
        return latest_words_that_fit(words, SINGLE_LINE_CHARACTERS);
    }

    let lines = wrap_caption_words(words, CAPTION_LINE_CHARACTERS);
    lines[lines.len().saturating_sub(keep)..].join("\n")
}

/// เก็บคำจากท้ายประโยคให้มากที่สุดโดยไม่ตัดคำกลางคัน
fn latest_words_that_fit(words: &[String], maximum_characters: usize) -> String {
    let mut start = words.len();
    let mut length = 0;

    while start > 0 {
        let word_length = words[start - 1].chars().count();
        let next_length = word_length + usize::from(start < words.len()) + length;
        if next_length > maximum_characters && start < words.len() {
            break;
        }
        start -= 1;
        length = next_length;
    }

    words[start..].join(" ")
}

/// ตัดบรรทัดแบบเติมให้เต็มก่อน แล้วเลือกจุดพักภาษาที่เหมาะสมเมื่อบรรทัดเต็ม
fn wrap_caption_words(words: &[String], maximum_characters: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut start = 0;

    while start < words.len() {
        let mut end = start;
        let mut line_length = 0;
        while end < words.len() {
            let next_length = line_length + usize::from(end > start) + words[end].chars().count();
            if next_length > maximum_characters && end > start {
                break;
            }
            line_length = next_length;
            end += 1;
            if line_length > maximum_characters {
                break;
            }
        }

        if end == words.len() {
            lines.push(words[start..end].join(" "));
            break;
        }

        let break_at = preferred_break(words, start, end);
        lines.push(words[start..break_at].join(" "));
        start = break_at;
    }

    lines
}

/// ให้คะแนนจบประโยค จบวลี และคำเชื่อมเพื่อหลีกเลี่ยงจุดตัดที่อ่านยาก
fn preferred_break(words: &[String], start: usize, greedy_end: usize) -> usize {
    let word_count = greedy_end - start;
    let earliest = start + word_count.div_ceil(2).max(1);
    let mut best = None;

    for break_at in earliest..=greedy_end {
        if break_at >= words.len() {
            break;
        }
        let score = if ends_sentence(&words[break_at - 1]) {
            4
        } else if ends_clause(&words[break_at - 1]) {
            3
        } else if starts_with_conjunction(&words[break_at]) {
            2
        } else {
            0
        };
        if score > 0 && best.is_none_or(|(best_score, _)| score >= best_score) {
            best = Some((score, break_at));
        }
    }
    if let Some((_, break_at)) = best {
        return break_at;
    }

    if greedy_end < words.len() && is_linked_pair(&words[greedy_end - 1], &words[greedy_end]) {
        for break_at in (earliest..greedy_end).rev() {
            if !is_linked_pair(&words[break_at - 1], &words[break_at]) {
                return break_at;
            }
        }
    }
    greedy_end
}

/// ตรวจเครื่องหมายจบประโยคโดยไม่นับจุดของคำนำหน้าชื่อและตัวย่อทั่วไป
fn ends_sentence(word: &str) -> bool {
    let word = word.trim_end_matches(|character| {
        matches!(character, '"' | '\'' | ')' | ']' | '}' | '’' | '”')
    });
    if word.ends_with('?') || word.ends_with('!') {
        return true;
    }
    if !word.ends_with('.') {
        return false;
    }
    !matches!(
        normalized_word(word).as_str(),
        "mr" | "mrs" | "ms" | "dr" | "prof" | "sr" | "jr" | "vs" | "eg" | "ie"
    )
}

/// ตรวจเครื่องหมายที่เหมาะกับการพักวลีภายในประโยค
fn ends_clause(word: &str) -> bool {
    let word = word.trim_end_matches(|character| {
        matches!(character, '"' | '\'' | ')' | ']' | '}' | '’' | '”')
    });
    word.ends_with(',') || word.ends_with(';') || word.ends_with(':') || word.ends_with('—')
}

/// ตรวจคำเชื่อมที่ควรเริ่มต้นบรรทัดใหม่เมื่อจำเป็นต้องตัด
fn starts_with_conjunction(word: &str) -> bool {
    matches!(
        normalized_word(word).as_str(),
        "and" | "but" | "or" | "so" | "because" | "although" | "though" | "while" | "yet"
    )
}

/// ป้องกันไม่ให้คำหน้าที่สั้น ๆ แยกจากคำถัดไปโดยไม่จำเป็น
fn is_linked_pair(left: &str, _right: &str) -> bool {
    matches!(
        normalized_word(left).as_str(),
        "a"
            | "an"
            | "the"
            | "to"
            | "of"
            | "in"
            | "on"
            | "at"
            | "for"
            | "from"
            | "with"
            | "by"
            | "i"
            | "you"
            | "he"
            | "she"
            | "it"
            | "we"
            | "they"
            | "is"
            | "are"
            | "was"
            | "were"
            | "be"
            | "been"
            | "being"
            | "have"
            | "has"
            | "had"
            | "do"
            | "does"
            | "did"
            | "can"
            | "could"
            | "will"
            | "would"
            | "should"
            | "not"
    )
}

/// ต่อคำใหม่โดยลบเฉพาะส่วนที่ซ้อนกับท้ายรายการเดิม
fn append_without_overlap(base: &mut Vec<String>, incoming: &[String]) {
    let overlap = longest_word_overlap(base, incoming);
    base.extend(incoming[overlap..].iter().cloned());
}

/// หาจำนวนคำยาวที่สุดที่ท้ายรายการเดิมตรงกับต้นรายการใหม่
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

/// หาจำนวนคำจากต้นที่เหมือนกันโดยไม่สนตัวพิมพ์เล็กใหญ่
fn common_prefix_len(left: &[String], right: &[String]) -> usize {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| left.eq_ignore_ascii_case(right))
        .count()
}

/// จัดแนวคำท้ายของข้อความเดิมกับสมมติฐานใหม่แม้ Whisper จะแทรกคำระหว่างกลาง
fn aligned_suffix_tail_start(base: &[String], incoming: &[String]) -> Option<usize> {
    for suffix_start in 0..base.len() {
        let mut incoming_start = 0;
        let mut last_match = None;
        let mut complete = true;
        for expected in &base[suffix_start..] {
            let Some(offset) = incoming[incoming_start..]
                .iter()
                .position(|word| normalized_word(word) == normalized_word(expected))
            else {
                complete = false;
                break;
            };
            incoming_start += offset + 1;
            last_match = Some(incoming_start);
        }
        if complete {
            return last_match;
        }
    }
    None
}

/// ลดรูปคำเพื่อเปรียบเทียบโดยตัดวรรคตอนรอบนอกและไม่สนตัวพิมพ์
fn normalized_word(word: &str) -> String {
    word.trim_matches(|character: char| !character.is_alphanumeric())
        .to_lowercase()
}

/// ทิ้งคำเก่าสุดจากด้านหน้าเมื่อประวัติยาวเกินขีดจำกัด
fn trim_front(words: &mut Vec<String>, maximum: usize) {
    if words.len() > maximum {
        words.drain(..words.len() - maximum);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TranscriptReconciler, format_live_caption, latest_words_that_fit, words,
        wrap_caption_words,
    };
    use crate::stt::TranscriptUpdate;

    #[test]
    fn stable_prefix_does_not_regress() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "I think we should");
        partial(&mut reconciler, 1, "I think we should go");
        partial(&mut reconciler, 1, "I think they might stay");

        assert_eq!(reconciler.presentation_text(), "I think we should stay");
        assert_eq!(reconciler.stable_word_count(), 4);
    }

    #[test]
    fn shifted_partial_retains_words_that_fell_off_the_window() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "one two three four");
        partial(&mut reconciler, 1, "three four five six");

        assert_eq!(
            reconciler.presentation_text(),
            "one two three four five six"
        );
    }

    #[test]
    fn repeated_words_at_the_stable_boundary_are_preserved() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "very very");
        partial(&mut reconciler, 1, "very very very");

        assert_eq!(reconciler.presentation_text(), "very very very");
    }

    #[test]
    fn final_after_committed_partial_does_not_duplicate_words() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "we should go now");
        partial(&mut reconciler, 1, "we should go now please");
        final_update(&mut reconciler, 1, "we should go now please");

        assert_eq!(reconciler.presentation_text(), "we should go now please");
    }

    #[test]
    fn revised_final_aligns_with_committed_words_without_duplication() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "we should go");
        partial(&mut reconciler, 1, "we should go now");
        final_update(&mut reconciler, 1, "we really should go home");

        assert_eq!(reconciler.presentation_text(), "we should go home");
    }

    #[test]
    fn duplicate_partial_and_final_events_are_idempotent() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 7, "Hello there.");
        partial(&mut reconciler, 7, "Hello there.");
        final_update(&mut reconciler, 7, "Hello there.");
        final_update(&mut reconciler, 7, "Hello there.");
        partial(&mut reconciler, 7, "Hello there.");

        assert_eq!(reconciler.presentation_text(), "Hello there.");
    }

    #[test]
    fn multiple_final_segments_accumulate() {
        let mut reconciler = TranscriptReconciler::default();

        final_update(&mut reconciler, 1, "This is the first sentence.");
        final_update(&mut reconciler, 2, "Here is the next one.");

        assert_eq!(
            reconciler.presentation_text(),
            "This is the first sentence. Here is the next one."
        );
    }

    #[test]
    fn presentation_trims_only_the_oldest_committed_words() {
        let mut reconciler = TranscriptReconciler::new(5);

        final_update(&mut reconciler, 1, "one two three");
        partial(&mut reconciler, 2, "four five six");

        assert_eq!(reconciler.presentation_text(), "two three four five six");
    }

    #[test]
    fn a_replaced_hypothesis_does_not_drop_its_new_prefix() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "one two");
        partial(&mut reconciler, 1, "one two three");
        partial(&mut reconciler, 1, "new phrase starts");

        assert_eq!(
            reconciler.presentation_text(),
            "one two three new phrase starts"
        );
    }

    #[test]
    fn revised_partial_punctuation_does_not_create_a_false_sentence_break() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "I think");
        partial(&mut reconciler, 1, "I think.");
        partial(&mut reconciler, 1, "I think this works");

        assert_eq!(reconciler.presentation_text(), "I think this works");
        assert_eq!(
            format_live_caption(reconciler.presentation_text(), 2),
            "I think this works"
        );
    }

    #[test]
    fn repeated_words_across_final_segments_are_preserved() {
        let mut reconciler = TranscriptReconciler::default();

        final_update(&mut reconciler, 1, "go");
        final_update(&mut reconciler, 2, "go home");

        assert_eq!(reconciler.presentation_text(), "go go home");
    }

    #[test]
    fn blank_audio_updates_do_not_replace_visible_text() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "Speech remains visible");
        assert!(!reconciler.apply(&TranscriptUpdate::Partial {
            segment_id: 1,
            text: "[blank_audio]".to_owned(),
        }));
        assert_eq!(reconciler.presentation_text(), "Speech remains visible");

        partial(
            &mut reconciler,
            1,
            "Speech [BLANK_AUDIO] remains visible now",
        );
        assert_eq!(
            reconciler.presentation_text(),
            "Speech remains visible now"
        );
    }

    #[test]
    fn completed_sentences_fill_available_lines_before_rollover() {
        assert_eq!(
            format_live_caption("Keep this completed sentence.", 2),
            "Keep this completed sentence."
        );
        assert_eq!(
            format_live_caption("Keep this completed sentence. Start", 2),
            "Keep this completed sentence. Start"
        );
        assert_eq!(format_live_caption("One. Two. Three.", 2), "One. Two. Three.");
        let rolled = format_live_caption(
            "Keep this completed sentence. Start the next caption with enough words to fill the whole line",
            2,
        );
        assert!(!rolled.contains("Keep this completed sentence."));
        assert!(rolled.contains("Start the next caption"));
        assert_eq!(
            format_live_caption("First sentence. Keep the latest sentence!", 2),
            "First sentence. Keep the latest sentence!"
        );
    }

    #[test]
    fn sliding_line_drops_only_the_oldest_words_when_full() {
        let caption_words =
            words("alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu");

        assert_eq!(
            latest_words_that_fit(&caption_words, 64),
            "beta gamma delta epsilon zeta eta theta iota kappa lambda mu"
        );
    }

    #[test]
    fn common_title_abbreviations_do_not_roll_the_sentence_over() {
        assert_eq!(
            format_live_caption("Dr. Smith is ready", 2),
            "Dr. Smith is ready"
        );
    }

    #[test]
    fn wrapping_is_explicit_stable_and_prefers_clause_boundaries() {
        let caption = format_live_caption(
            "This is a carefully prepared example, and it keeps moving toward the ending",
            2,
        );
        let lines = caption.lines().collect::<Vec<_>>();

        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with(','));
        assert!(lines.iter().all(|line| line.chars().count() <= 42));
        assert_eq!(
            format_live_caption(
                "This is a carefully prepared example, and it keeps moving toward the final ending",
                2,
            )
            .lines()
            .next(),
            Some(lines[0])
        );
    }

    #[test]
    fn wrapping_keeps_obvious_linked_words_together() {
        assert_eq!(
            wrap_caption_words(&words("please see the screen"), 14),
            vec!["please see".to_owned(), "the screen".to_owned()]
        );
    }

    #[test]
    fn long_unpunctuated_speech_rolls_old_lines_away() {
        let caption = format_live_caption(
            "old words should leave the screen as this uninterrupted live sentence continues with enough additional speech to create several caption lines for the viewer right now",
            2,
        );

        assert_eq!(caption.lines().count(), 2);
        assert!(!caption.contains("old words"));
        assert!(caption.ends_with("right now"));
    }

    fn partial(reconciler: &mut TranscriptReconciler, segment_id: u64, text: &str) {
        reconciler.apply(&TranscriptUpdate::Partial {
            segment_id,
            text: text.to_owned(),
        });
    }

    fn final_update(reconciler: &mut TranscriptReconciler, segment_id: u64, text: &str) {
        reconciler.apply(&TranscriptUpdate::Final {
            segment_id,
            text: text.to_owned(),
        });
    }
}
