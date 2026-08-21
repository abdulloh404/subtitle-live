//! การรวมข้อความถอดเสียงแบบชั่วคราวและยืนยันแล้ว พร้อมสถานะที่ใช้แสดงเป็นคำบรรยาย

use std::collections::VecDeque;

use crate::stt::{TranscriptUpdate, normalize_hypothesis_text};

// จำกัดประวัติที่แสดงเพื่อไม่ให้ข้อความสะสมเพิ่มหน่วยความจำอย่างไม่มีขอบเขต
const DEFAULT_PRESENTATION_WORDS: usize = 64;
// เก็บบรรทัดที่ปิดแล้วมากกว่าค่าสูงสุดของ UI เล็กน้อยเพื่อดันบรรทัดได้ต่อเนื่อง
const MAX_RETAINED_CAPTION_LINES: usize = 8;

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

/// คิวบรรทัดคำบรรยายที่ปิดบรรทัดเดิมทันทีเมื่อคำถัดไปเกินความกว้างจริง
#[derive(Debug, Default)]
pub(crate) struct CaptionLineBuffer {
    /// บรรทัดที่เต็มแล้วและจะไม่ถูกผลชั่วคราวของ Whisper แก้ย้อนหลัง
    committed_lines: VecDeque<Vec<String>>,
    /// บรรทัดล่าสุดที่ยังรับคำใหม่และแก้ผลชั่วคราวได้
    active_line: Vec<String>,
    /// ข้อความต้นทางจากรอบก่อนสำหรับตรวจการเลื่อนหน้าต่างประวัติ
    previous_words: Vec<String>,
    /// ตำแหน่งเริ่มบรรทัดล่าสุดภายในข้อความต้นทาง
    active_start: usize,
}

impl CaptionLineBuffer {
    /// อัปเดตคิวบรรทัด โดยผู้เรียกเป็นผู้วัดว่าข้อความหนึ่งบรรทัดพอดีกับความกว้างหรือไม่
    pub(crate) fn update<F>(&mut self, text: &str, max_lines: u32, mut fits: F) -> String
    where
        F: FnMut(&str) -> bool,
    {
        let incoming = words(text);
        if incoming.is_empty() {
            return self.presentation(max_lines);
        }

        if !self.previous_words.is_empty() {
            let common_prefix = common_prefix_len(&self.previous_words, &incoming);
            if common_prefix == 0 {
                let shifted_overlap = longest_word_overlap(&self.previous_words, &incoming);
                if shifted_overlap >= 3 && shifted_overlap < self.previous_words.len() {
                    let removed_prefix = self.previous_words.len() - shifted_overlap;
                    self.active_start = self.active_start.saturating_sub(removed_prefix);
                }
            }
        }

        // ผลชั่วคราวที่สั้นถอยข้ามบรรทัดที่ปิดแล้วต้องไม่ดึงบรรทัดเก่ากลับลงมา
        if incoming.len() < self.active_start
            || (incoming.len() == self.active_start && !self.active_line.is_empty())
        {
            return self.presentation(max_lines);
        }

        self.active_line = incoming[self.active_start..].to_vec();
        self.previous_words = incoming;
        let wrapped = wrap_words_to_width(&self.active_line, &mut fits);
        if wrapped.len() > 1 {
            for line in &wrapped[..wrapped.len() - 1] {
                self.active_start = self.active_start.saturating_add(line.len());
                self.committed_lines.push_back(line.clone());
            }
            self.active_line = wrapped.last().cloned().unwrap_or_default();
        }

        while self.committed_lines.len() > MAX_RETAINED_CAPTION_LINES {
            self.committed_lines.pop_front();
        }
        self.presentation(max_lines)
    }

    /// ล้างขอบเขตบรรทัดเมื่อซ่อน overlay หรือเปลี่ยนรูปแบบที่มีผลต่อความกว้าง
    pub(crate) fn clear(&mut self) {
        self.committed_lines.clear();
        self.active_line.clear();
        self.previous_words.clear();
        self.active_start = 0;
    }

    /// คืนบรรทัดล่าสุดตามจำนวนที่กำหนด โดยดันบรรทัดเก่าขึ้นและทิ้งจากหน้าจอ
    fn presentation(&self, max_lines: u32) -> String {
        let mut lines = self
            .committed_lines
            .iter()
            .map(|line| line.join(" "))
            .collect::<Vec<_>>();
        if !self.active_line.is_empty() {
            lines.push(self.active_line.join(" "));
        }
        let keep = max_lines.max(1) as usize;
        lines[lines.len().saturating_sub(keep)..].join("\n")
    }
}

/// เติมคำลงบรรทัดให้มากที่สุดและตัดเฉพาะตรงช่องว่างเมื่อคำถัดไปเกินความกว้าง
fn wrap_words_to_width<F>(words: &[String], fits: &mut F) -> Vec<Vec<String>>
where
    F: FnMut(&str) -> bool,
{
    let mut lines = Vec::new();
    let mut start = 0;
    while start < words.len() {
        let mut end = start;
        while end < words.len() {
            let candidate = words[start..=end].join(" ");
            if end > start && !fits(&candidate) {
                break;
            }
            end += 1;
        }
        lines.push(words[start..end].to_vec());
        start = end;
    }
    lines
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
    use super::{CaptionLineBuffer, TranscriptReconciler};
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
        assert_eq!(reconciler.presentation_text(), "Speech remains visible now");
    }

    #[test]
    fn one_line_replaces_the_whole_line_after_reaching_width() {
        let mut buffer = CaptionLineBuffer::default();

        assert_eq!(caption(&mut buffer, "one two", 1, 7), "one two");
        assert_eq!(caption(&mut buffer, "one two three", 1, 7), "three");
        assert_eq!(caption(&mut buffer, "one two", 1, 7), "three");
        assert_eq!(caption(&mut buffer, "one two four", 1, 7), "four");
    }

    #[test]
    fn two_lines_push_the_older_line_up_when_the_limit_is_full() {
        let mut buffer = CaptionLineBuffer::default();

        assert_eq!(
            caption(&mut buffer, "one two three", 2, 7),
            "one two\nthree"
        );
        assert_eq!(
            caption(&mut buffer, "one two three four", 2, 7),
            "three\nfour"
        );
    }

    #[test]
    fn a_closed_line_is_not_rewritten_by_a_later_partial_result() {
        let mut buffer = CaptionLineBuffer::default();

        assert_eq!(
            caption(&mut buffer, "one two three", 2, 7),
            "one two\nthree"
        );
        assert_eq!(
            caption(&mut buffer, "one too three", 2, 7),
            "one two\nthree"
        );
    }

    #[test]
    fn punctuation_does_not_cut_a_line_before_the_width_limit() {
        let mut buffer = CaptionLineBuffer::default();

        assert_eq!(
            caption(&mut buffer, "First. second third", 2, 19),
            "First. second third"
        );
    }

    fn caption(
        buffer: &mut CaptionLineBuffer,
        text: &str,
        max_lines: u32,
        maximum_characters: usize,
    ) -> String {
        buffer.update(text, max_lines, |candidate| {
            candidate.chars().count() <= maximum_characters
        })
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
