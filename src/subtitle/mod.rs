//! การรวมข้อความถอดเสียงแบบชั่วคราวและยืนยันแล้ว พร้อมสถานะที่ใช้แสดงเป็นคำบรรยาย

use std::collections::VecDeque;

use crate::stt::{TranscriptUpdate, normalize_hypothesis_text};

// จำกัดประวัติที่แสดงเพื่อไม่ให้ข้อความสะสมเพิ่มหน่วยความจำอย่างไม่มีขอบเขต
const DEFAULT_PRESENTATION_WORDS: usize = 64;
// เก็บบรรทัดที่ปิดแล้วมากกว่าค่าสูงสุดของ UI เล็กน้อยเพื่อดันบรรทัดได้ต่อเนื่อง
const MAX_RETAINED_CAPTION_LINES: usize = 8;
// ต้องมีคำซ้อนกันพอสมควรก่อนถือว่า hypothesis ใหม่เป็นหน้าต่างเดิมที่เลื่อนไปทางขวา
const MIN_ROLLING_OVERLAP_WORDS: usize = 2;
/// รวมผลยืนยันกับสมมติฐานล่าสุดที่ Whisper ยังแก้ไขได้
#[derive(Debug)]
pub struct TranscriptReconciler {
    /// คำจากช่วงที่ยืนยันแล้วก่อนหน้า ซึ่งจะไม่ถูกแก้โดยผลชั่วคราวใหม่
    finalized: Vec<String>,
    /// สมมติฐานล่าสุดของช่วงปัจจุบัน ซึ่งผลชั่วคราวรอบถัดไปแทนที่ได้ทั้งก้อน
    draft: Vec<String>,
    /// ประโยคก่อนหน้าของ segment ปัจจุบันที่เลื่อนพ้นหน้าต่างเสียงและไม่ควรหายจากจอ
    active_prefix: Vec<String>,
    /// รหัสช่วงผลชั่วคราวที่กำลังประมวลผล
    active_segment_id: Option<u64>,
    /// รหัสผลยืนยันล่าสุด ใช้ปฏิเสธเหตุการณ์ซ้ำหรือมาช้า
    last_final_segment_id: Option<u64>,
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
            draft: Vec::new(),
            active_prefix: Vec::new(),
            active_segment_id: None,
            last_final_segment_id: None,
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

    /// คืนจำนวนคำจากประโยคก่อนหน้าของช่วงปัจจุบันที่รักษาไว้ไม่ให้ rolling window ลบ
    pub fn stable_word_count(&self) -> usize {
        self.active_prefix.len()
    }

    /// ล้างทุกช่วงข้อความเมื่อหยุดกระบวนการหรือเปลี่ยนแหล่งเสียง
    pub fn clear(&mut self) {
        self.finalized.clear();
        self.draft.clear();
        self.active_prefix.clear();
        self.active_segment_id = None;
        self.last_final_segment_id = None;
        self.presentation.clear();
    }

    /// เก็บผลชั่วคราวล่าสุดเป็น draft ทั้งก้อนโดยไม่ยืนยันคำนำหน้าก่อนเวลา
    fn apply_partial(&mut self, segment_id: u64, incoming: Vec<String>) {
        if self
            .last_final_segment_id
            .is_some_and(|finalized| segment_id <= finalized)
            || self
                .active_segment_id
                .is_some_and(|active| segment_id < active)
        {
            return;
        }

        if self.active_segment_id != Some(segment_id) {
            self.commit_active_without_final();
            self.active_segment_id = Some(segment_id);
            self.set_active_hypothesis(incoming);
            return;
        }

        let tail = reconcile_mutable_tail(&self.active_prefix, &self.draft, &incoming, true, true);
        self.set_active_tail(tail);
    }

    /// ปิดช่วงปัจจุบันและ commit ผลยืนยันเพียงครั้งเดียวตามรหัส segment
    fn apply_final(&mut self, segment_id: u64, incoming: Vec<String>) {
        if self
            .last_final_segment_id
            .is_some_and(|finalized| segment_id <= finalized)
            || self
                .active_segment_id
                .is_some_and(|active| segment_id < active)
        {
            return;
        }
        if self
            .active_segment_id
            .is_some_and(|active| active < segment_id)
        {
            self.commit_active_without_final();
        }
        let segment = if self.active_segment_id == Some(segment_id) {
            if incoming.is_empty() {
                let mut fallback = self.active_prefix.clone();
                fallback.extend(self.draft.iter().cloned());
                fallback
            } else {
                let mut segment = self.active_prefix.clone();
                let tail = reconcile_mutable_tail(
                    &self.active_prefix,
                    &self.draft,
                    &incoming,
                    false,
                    true,
                );
                segment.extend(tail);
                segment
            }
        } else {
            incoming
        };
        self.last_final_segment_id = Some(segment_id);
        self.finalized.extend(segment);
        trim_front(&mut self.finalized, self.max_presentation_words);
        self.clear_active();
    }

    /// ล้างสถานะเฉพาะช่วงปัจจุบันหลังย้ายคำไปยังประวัติที่ยืนยันแล้ว
    fn clear_active(&mut self) {
        self.active_prefix.clear();
        self.draft.clear();
        self.active_segment_id = None;
    }

    /// แยกประโยคที่จบแล้วออกจากส่วนท้ายที่ยังให้ผล Partial รอบถัดไปแก้ไขได้
    fn set_active_hypothesis(&mut self, hypothesis: Vec<String>) {
        let stable_end = last_sentence_boundary(&hypothesis);
        self.active_prefix = hypothesis[..stable_end].to_vec();
        self.draft = hypothesis[stable_end..].to_vec();
        trim_front(&mut self.active_prefix, self.max_presentation_words);
    }

    /// เพิ่มเฉพาะประโยคที่ปิดใหม่จาก mutable tail โดยไม่แก้ active prefix เดิม
    fn set_active_tail(&mut self, tail: Vec<String>) {
        let stable_end = last_sentence_boundary(&tail);
        self.active_prefix
            .extend(tail[..stable_end].iter().cloned());
        self.draft = tail[stable_end..].to_vec();
        trim_front(&mut self.active_prefix, self.max_presentation_words);
    }

    /// เก็บสมมติฐานที่ดีที่สุดเมื่อ segment เปลี่ยนก่อน Whisper ส่ง Final
    fn commit_active_without_final(&mut self) {
        if self.active_segment_id.is_none() {
            return;
        }

        let mut segment = self.active_prefix.clone();
        segment.extend(self.draft.iter().cloned());
        self.finalized.extend(segment);
        trim_front(&mut self.finalized, self.max_presentation_words);
        self.clear_active();
    }

    /// ประกอบคำที่ยืนยันแล้วกับ draft ล่าสุดตามลำดับเวลา
    fn refresh_presentation(&mut self) {
        let mut combined = self.finalized.clone();
        combined.extend(self.active_prefix.iter().cloned());
        combined.extend(self.draft.iter().cloned());
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

/// แยก mutable tail ออกจาก hypothesis ใหม่โดยไม่ให้ประโยคที่ปิดแล้วถูกแก้หรือถูกต่อซ้ำ
fn reconcile_mutable_tail(
    stable: &[String],
    draft: &[String],
    incoming: &[String],
    preserve_prefix_shrink: bool,
    preserve_unaligned_shrink: bool,
) -> Vec<String> {
    if !stable.is_empty() {
        let stable_prefix = common_prefix_len(stable, incoming);
        if stable_prefix == incoming.len() {
            // สมมติฐานสั้นที่ย้อนกลับไปมีเพียงประโยคที่ปิดแล้วต้องไม่ลบ draft
            // ซึ่งยังแสดงอยู่ รอผลที่ต่อเนื่องหรือ Final มาตัดสินแทน
            return reconciled_partial_tail(draft, &[], preserve_prefix_shrink);
        }
        if stable_prefix == stable.len() {
            return reconciled_partial_tail(
                draft,
                &incoming[stable_prefix..],
                preserve_prefix_shrink,
            );
        }

        // Rolling window อาจเริ่มกลางประโยคที่อยู่ท้าย stable prefix แล้ว จึงต้อง
        // ตัดส่วนที่ซ้ำออกแทนการ commit ประโยคเดิมซ้ำอีกครั้ง
        let stable_overlap = longest_word_overlap(stable, incoming);
        if meaningful_overlap(stable, incoming, stable_overlap) {
            return reconciled_partial_tail(
                draft,
                &incoming[stable_overlap..],
                preserve_prefix_shrink,
            );
        }

        // บางรอบ Whisper เติมคำหลงมาหนึ่งคำหน้าประโยคเก่าที่เลื่อนพ้น window
        // ต้องข้ามคำนั้นก่อนตรวจ suffix เพื่อไม่ให้ประโยคเดิมกลับมาแสดงซ้ำชั่วคราว
        if let Some(tail_start) = stable_tail_start_after_leading_word(stable, incoming) {
            return reconciled_partial_tail(draft, &incoming[tail_start..], preserve_prefix_shrink);
        }
    }

    // Whisper บางรอบย้อนกลับมาเหลือเพียง prefix ของ draft เดิมก่อนจะต่อคำใหม่
    // การรับผลนั้นทันทีทำให้ subtitle หด ทั้งที่ยังไม่มีคำแก้ไขมาหักล้างข้อความเดิม
    let draft_prefix = common_prefix_len(draft, incoming);
    if preserve_prefix_shrink && incoming.len() < draft.len() && draft_prefix == incoming.len() {
        return draft.to_vec();
    }

    let shifted_overlap = longest_word_overlap(draft, incoming);
    if meaningful_overlap(draft, incoming, shifted_overlap) {
        let mut merged = draft[..draft.len() - shifted_overlap].to_vec();
        merged.extend(incoming.iter().cloned());
        return merged;
    }

    // Whisper อาจแก้คำสุดท้ายพร้อมกับเลื่อนหน้าต่าง เช่น
    // `... consistent with creation` -> `consistent with creating` ทำให้ suffix
    // ไม่ตรงทั้งหมด หา prefix ของผลใหม่ภายใน draft เดิมเพื่อรักษาคำที่เลื่อนพ้นจอ
    if preserve_prefix_shrink {
        if let Some(start) = shifted_window_start(draft, incoming) {
            let mut merged = draft[..start].to_vec();
            merged.extend(incoming.iter().cloned());
            return merged;
        }
    }

    if let Some(start) = aligned_draft_start(draft, incoming) {
        return incoming[start..].to_vec();
    }

    // ผลที่สั้นกว่าแต่หาแนวต่อไม่ได้ยังไม่มีหลักฐานพอให้ลบคำเดิม โดยเฉพาะเมื่อ
    // rolling window เลื่อนต้นประโยคออกไปหรือ Whisper เปลี่ยนรูปประโยคชั่วคราว
    if preserve_unaligned_shrink && incoming.len() < draft.len() {
        return draft.to_vec();
    }

    incoming.to_vec()
}

/// รักษา draft เดิมเมื่อ hypothesis ใหม่เป็นเพียง prefix ที่สั้นกว่า
fn reconciled_partial_tail(
    draft: &[String],
    incoming_tail: &[String],
    preserve_prefix_shrink: bool,
) -> Vec<String> {
    if preserve_prefix_shrink
        && incoming_tail.len() < draft.len()
        && common_prefix_len(draft, incoming_tail) == incoming_tail.len()
    {
        draft.to_vec()
    } else {
        incoming_tail.to_vec()
    }
}

/// หาจุดเริ่ม draft เดิมภายใน hypothesis ใหม่เพื่อทิ้งฉบับแก้ของประโยคที่ปิดแล้ว
fn aligned_draft_start(draft: &[String], incoming: &[String]) -> Option<usize> {
    let mut best = None;
    for start in 0..incoming.len() {
        let overlap = common_prefix_len(draft, &incoming[start..]);
        if overlap >= MIN_ROLLING_OVERLAP_WORDS
            || (overlap == 1 && (start == 0 || draft.len() == 1))
        {
            if best.map_or(true, |(_, best_overlap)| overlap > best_overlap) {
                best = Some((start, overlap));
            }
        }
    }
    best.map(|(start, _)| start)
}

/// หาจุดที่ต้น hypothesis ใหม่อยู่ภายใน draft เดิม แม้คำท้ายเดิมถูกแก้ในรอบเดียวกับที่ window เลื่อน
fn shifted_window_start(draft: &[String], incoming: &[String]) -> Option<usize> {
    let mut best = None;
    for start in 1..draft.len() {
        let overlap = common_prefix_len(&draft[start..], incoming);
        if overlap >= MIN_ROLLING_OVERLAP_WORDS
            && best.is_none_or(|(best_start, best_overlap)| {
                overlap > best_overlap || (overlap == best_overlap && start > best_start)
            })
        {
            best = Some((start, overlap));
        }
    }
    best.map(|(start, _)| start)
}

/// คืนตำแหน่ง tail เมื่อ hypothesis มีคำหลงหนึ่งคำแล้วตามด้วย suffix ของประโยคที่ปิดแล้ว
fn stable_tail_start_after_leading_word(stable: &[String], incoming: &[String]) -> Option<usize> {
    let overlap = longest_word_overlap(stable, incoming.get(1..)?);
    (overlap >= 3).then_some(overlap + 1)
}

/// หาจำนวนคำต้นที่เหมือนกันโดยไม่สนตัวพิมพ์และวรรคตอน
fn common_prefix_len(previous: &[String], incoming: &[String]) -> usize {
    previous
        .iter()
        .zip(incoming)
        .take_while(|(left, right)| normalized_word(left) == normalized_word(right))
        .count()
}

/// หาจำนวนคำยาวที่สุดที่ท้าย hypothesis เดิมตรงกับต้น hypothesis ใหม่
fn longest_word_overlap(previous: &[String], incoming: &[String]) -> usize {
    let maximum = previous.len().min(incoming.len());
    (1..=maximum)
        .rev()
        .find(|&length| {
            previous[previous.len() - length..]
                .iter()
                .zip(&incoming[..length])
                .all(|(left, right)| normalized_word(left) == normalized_word(right))
        })
        .unwrap_or(0)
}

/// ลดรูปคำสำหรับตรวจหน้าต่างเลื่อนโดยไม่ให้ตัวพิมพ์หรือวรรคตอนท้ายรบกวนการจับคู่
fn normalized_word(word: &str) -> String {
    word.trim_matches(|character: char| !character.is_alphanumeric())
        .to_lowercase()
}

/// ยอมรับ overlap สองคำขึ้นไป หรือหนึ่งคำเมื่อ hypothesis เดิมยังสั้นหรือจบประโยคชัดเจน
fn meaningful_overlap(previous: &[String], incoming: &[String], overlap: usize) -> bool {
    overlap >= MIN_ROLLING_OVERLAP_WORDS
        || (overlap == 1
            && ((previous.len() <= MIN_ROLLING_OVERLAP_WORDS && incoming.len() > 1)
                || previous
                    .last()
                    .is_some_and(|word| is_sentence_boundary_word(word))))
}

/// คืนตำแหน่งหลังคำจบประโยคล่าสุด โดยไม่ย้ายเศษประโยคปัจจุบันไปส่วนคงที่
fn last_sentence_boundary(words: &[String]) -> usize {
    let completed = words
        .get(..words.len().saturating_sub(1))
        .unwrap_or_default();
    completed
        .iter()
        .rposition(|word| is_sentence_boundary_word(word))
        .map_or(0, |index| index + 1)
}

/// แยกเครื่องหมายจบประโยคจริงออกจากตัวย่อทั่วไปและเลขที่มีจุดภายใน
fn is_sentence_boundary_word(word: &str) -> bool {
    let word = word.trim_end_matches(|character: char| {
        matches!(character, '"' | '\'' | '’' | ')' | ']' | '}')
    });
    if word.ends_with(['!', '?']) {
        return true;
    }
    if !word.ends_with('.') {
        return false;
    }

    let stem = word.trim_end_matches('.').to_ascii_lowercase();
    !stem.contains('.')
        && !matches!(
            stem.as_str(),
            "mr" | "mrs" | "ms" | "dr" | "prof" | "sr" | "jr" | "st" | "vs" | "etc"
        )
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
        let mut incoming = words(text);
        if incoming.is_empty() {
            return self.presentation(max_lines);
        }

        if !self.previous_words.is_empty() {
            let common_prefix = common_prefix_len(&self.previous_words, &incoming);
            let mut history_aligned = common_prefix > 0;
            if common_prefix == 0 {
                let shifted_overlap = longest_word_overlap(&self.previous_words, &incoming);
                if shifted_overlap >= 3 && shifted_overlap < self.previous_words.len() {
                    history_aligned = true;
                    let removed_prefix = self.previous_words.len() - shifted_overlap;
                    if removed_prefix > self.active_start {
                        // คำที่หลุดจาก source ยังอยู่บนบรรทัดปัจจุบัน จึงรักษาไว้และต่อเฉพาะ suffix ใหม่
                        let mut stabilized = self.previous_words.clone();
                        stabilized.extend(incoming[shifted_overlap..].iter().cloned());
                        incoming = stabilized;
                    } else {
                        self.active_start -= removed_prefix;
                    }
                }
            }

            // ผลที่สั้นลงแต่ยังอยู่ในหน้าต่างเดิมต้องไม่ดึงคำที่ผู้ใช้อ่านแล้วออกจากจอ
            if history_aligned && incoming.len() < self.previous_words.len() {
                return self.presentation(max_lines);
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
    fn each_partial_replaces_the_entire_draft() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "I think we should");
        partial(&mut reconciler, 1, "I think we should go");
        partial(&mut reconciler, 1, "I think they might stay");

        assert_eq!(reconciler.presentation_text(), "I think they might stay");
        assert_eq!(reconciler.stable_word_count(), 0);
    }

    #[test]
    fn two_word_overlap_keeps_the_text_that_left_the_window() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "one two three four");
        partial(&mut reconciler, 1, "three four five six");

        assert_eq!(
            reconciler.presentation_text(),
            "one two three four five six"
        );
    }

    #[test]
    fn one_word_overlap_keeps_the_text_that_left_the_window() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "one two");
        partial(&mut reconciler, 1, "two three");

        assert_eq!(reconciler.presentation_text(), "one two three");
    }

    #[test]
    fn coincidental_one_word_overlap_replaces_a_longer_hypothesis() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "we should go now");
        partial(&mut reconciler, 1, "now this is different");

        assert_eq!(reconciler.presentation_text(), "now this is different");
    }

    #[test]
    fn rolling_shift_with_a_revised_last_word_keeps_the_earlier_phrase() {
        let mut reconciler = TranscriptReconciler::default();

        partial(
            &mut reconciler,
            1,
            "I'm going to get better at this. My goal is to be more regular and consistent with creation",
        );
        partial(
            &mut reconciler,
            1,
            "more regular and consistent with creating",
        );

        assert_eq!(
            reconciler.presentation_text(),
            "I'm going to get better at this. My goal is to be more regular and consistent with creating"
        );
    }

    #[test]
    fn short_partial_from_the_stable_prefix_does_not_clear_the_live_draft() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "First sentence. still speaking clearly");
        partial(&mut reconciler, 1, "First sentence. still");
        assert_eq!(
            reconciler.presentation_text(),
            "First sentence. still speaking clearly"
        );

        partial(&mut reconciler, 1, "First sentence");

        assert_eq!(
            reconciler.presentation_text(),
            "First sentence. still speaking clearly"
        );
    }

    #[test]
    fn shorter_unaligned_partial_does_not_erase_the_existing_draft() {
        let mut reconciler = TranscriptReconciler::default();

        partial(
            &mut reconciler,
            1,
            "My name is Alba and I'm an English teacher",
        );
        partial(&mut reconciler, 1, "I'm Alba and I'm an English teacher");

        assert_eq!(
            reconciler.presentation_text(),
            "My name is Alba and I'm an English teacher"
        );
    }

    #[test]
    fn shorter_unaligned_final_does_not_erase_the_existing_draft() {
        let mut reconciler = TranscriptReconciler::default();

        partial(
            &mut reconciler,
            1,
            "My name is Alba and I'm an English teacher",
        );
        final_update(&mut reconciler, 1, "I'm Alba and I'm an English teacher");

        assert_eq!(
            reconciler.presentation_text(),
            "My name is Alba and I'm an English teacher"
        );
    }

    #[test]
    fn final_after_a_revised_window_shift_does_not_restore_a_missing_prefix_in_a_burst() {
        let mut reconciler = TranscriptReconciler::default();

        partial(
            &mut reconciler,
            1,
            "So today I wanted to share with you what I do to practice and",
        );
        partial(&mut reconciler, 1, "with you what I do to practice English");
        assert_eq!(
            reconciler.presentation_text(),
            "So today I wanted to share with you what I do to practice English"
        );

        final_update(
            &mut reconciler,
            1,
            "So today I wanted to share with you what I do to practice English.",
        );
        assert_eq!(
            reconciler.presentation_text(),
            "So today I wanted to share with you what I do to practice English."
        );
    }

    #[test]
    fn shorter_final_removes_the_obsolete_partial_word() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "we should go home");
        final_update(&mut reconciler, 1, "we should go");

        assert_eq!(reconciler.presentation_text(), "we should go");
    }

    #[test]
    fn final_revision_does_not_keep_an_internal_partial_prefix() {
        let mut reconciler = TranscriptReconciler::default();

        partial(
            &mut reconciler,
            1,
            "we should go because I really should stay",
        );
        final_update(&mut reconciler, 1, "I really should leave");

        assert_eq!(reconciler.presentation_text(), "I really should leave");
    }

    #[test]
    fn suffix_overlap_wins_over_a_coincidental_first_word_match() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "I was there and I think");
        partial(&mut reconciler, 1, "I think we should continue");

        assert_eq!(
            reconciler.presentation_text(),
            "I was there and I think we should continue"
        );
    }

    #[test]
    fn completed_sentence_stays_visible_when_the_window_shifts() {
        let mut reconciler = TranscriptReconciler::default();

        partial(
            &mut reconciler,
            1,
            "First sentence ends here. current words keep moving",
        );
        partial(&mut reconciler, 1, "current words keep moving forward");

        assert_eq!(
            reconciler.presentation_text(),
            "First sentence ends here. current words keep moving forward"
        );
        assert_eq!(reconciler.stable_word_count(), 4);
    }

    #[test]
    fn revision_inside_a_stable_sentence_updates_only_the_mutable_tail() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "I like blue cars. Next");
        partial(&mut reconciler, 1, "I like red cars. Next word");

        assert_eq!(
            reconciler.presentation_text(),
            "I like blue cars. Next word"
        );
    }

    #[test]
    fn revised_final_does_not_append_a_second_version_of_the_stable_sentence() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "I like blue cars. Next");
        final_update(&mut reconciler, 1, "I like red cars.");

        assert_eq!(reconciler.presentation_text(), "I like blue cars.");
    }

    #[test]
    fn unfinished_words_before_the_overlap_remain_in_the_live_draft() {
        let mut reconciler = TranscriptReconciler::default();

        partial(
            &mut reconciler,
            1,
            "A complete thought. this phrase keeps moving slowly",
        );
        partial(&mut reconciler, 1, "phrase keeps moving slowly forward");

        assert_eq!(
            reconciler.presentation_text(),
            "A complete thought. this phrase keeps moving slowly forward"
        );
        assert_eq!(reconciler.stable_word_count(), 3);
    }

    #[test]
    fn one_word_sentence_overlap_does_not_repeat_the_boundary() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "Practice, right? You have to speak");
        partial(&mut reconciler, 1, "Right? You have to speak now");

        assert_eq!(
            reconciler.presentation_text(),
            "Practice, right? You have to speak now"
        );
    }

    #[test]
    fn stable_sentence_suffix_is_not_committed_twice_after_a_transient_partial() {
        let mut reconciler = TranscriptReconciler::default();

        partial(
            &mut reconciler,
            1,
            "Hi everyone. Long time no see, right? I know I",
        );
        partial(&mut reconciler, 1, "a long time no see, right");
        assert_eq!(
            reconciler.presentation_text(),
            "Hi everyone. Long time no see, right? I know I"
        );

        partial(
            &mut reconciler,
            1,
            "Long time no see, right? I know I haven't",
        );

        assert_eq!(
            reconciler.presentation_text(),
            "Hi everyone. Long time no see, right? I know I haven't"
        );
    }

    #[test]
    fn segment_change_without_final_keeps_the_best_draft() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "listening to English alone will");
        partial(&mut reconciler, 2, "That will improve your speaking");

        assert_eq!(
            reconciler.presentation_text(),
            "listening to English alone will That will improve your speaking"
        );
    }

    #[test]
    fn distinct_segments_preserve_repeated_words_at_the_boundary() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "we need to go now");
        partial(&mut reconciler, 2, "to go now please");

        assert_eq!(
            reconciler.presentation_text(),
            "we need to go now to go now please"
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
    fn final_replaces_the_draft_and_commits_once() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "we should go now");
        partial(&mut reconciler, 1, "we should go now please");
        final_update(&mut reconciler, 1, "we should go now please");

        assert_eq!(reconciler.presentation_text(), "we should go now please");
    }

    #[test]
    fn revised_final_replaces_the_partial_wording() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "we should go");
        partial(&mut reconciler, 1, "we should go now");
        final_update(&mut reconciler, 1, "we really should go home");

        assert_eq!(reconciler.presentation_text(), "we really should go home");
    }

    #[test]
    fn final_keeps_a_stable_sentence_that_left_the_final_window() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "First sentence. still speaking");
        final_update(&mut reconciler, 1, "still speaking clearly.");

        assert_eq!(
            reconciler.presentation_text(),
            "First sentence. still speaking clearly."
        );
    }

    #[test]
    fn ambiguous_final_preserves_all_incoming_sentences_after_the_stable_prefix() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "Old sentence. draft");
        final_update(&mut reconciler, 1, "Revised sentence. Brand new sentence.");

        assert_eq!(
            reconciler.presentation_text(),
            "Old sentence. Revised sentence. Brand new sentence."
        );
    }

    #[test]
    fn ambiguous_final_keeps_a_brand_new_sentence_after_unfinished_draft() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "Old sentence. unfinished words");
        final_update(&mut reconciler, 1, "Brand new sentence.");

        assert_eq!(
            reconciler.presentation_text(),
            "Old sentence. Brand new sentence."
        );
    }

    #[test]
    fn shorter_final_does_not_truncate_the_stable_prefix() {
        let mut reconciler = TranscriptReconciler::default();

        partial(
            &mut reconciler,
            1,
            "First sentence. Second sentence. still speaking",
        );
        final_update(&mut reconciler, 1, "First sentence.");

        assert_eq!(
            reconciler.presentation_text(),
            "First sentence. Second sentence."
        );
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
    fn a_partial_phrase_repeated_across_real_final_boundaries_is_preserved() {
        let mut reconciler = TranscriptReconciler::default();

        final_update(&mut reconciler, 1, "hello there everyone");
        final_update(&mut reconciler, 2, "there everyone welcome back");

        assert_eq!(
            reconciler.presentation_text(),
            "hello there everyone there everyone welcome back"
        );
    }

    #[test]
    fn repeated_sentence_in_a_new_segment_is_preserved() {
        let mut reconciler = TranscriptReconciler::default();

        final_update(&mut reconciler, 1, "Hello there.");
        final_update(&mut reconciler, 2, "Hello there.");

        assert_eq!(reconciler.presentation_text(), "Hello there. Hello there.");
    }

    #[test]
    fn presentation_trims_only_the_oldest_committed_words() {
        let mut reconciler = TranscriptReconciler::new(5);

        final_update(&mut reconciler, 1, "one two three");
        partial(&mut reconciler, 2, "four five six");

        assert_eq!(reconciler.presentation_text(), "two three four five six");
    }

    #[test]
    fn a_replaced_hypothesis_drops_the_obsolete_draft() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "one two");
        partial(&mut reconciler, 1, "one two three");
        partial(&mut reconciler, 1, "new phrase starts");

        assert_eq!(reconciler.presentation_text(), "new phrase starts");
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
    fn empty_final_keeps_the_best_available_draft() {
        let mut reconciler = TranscriptReconciler::default();

        partial(&mut reconciler, 1, "Maybe this was speech");
        final_update(&mut reconciler, 1, "[BLANK_AUDIO]");

        assert_eq!(reconciler.presentation_text(), "Maybe this was speech");
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
    fn words_trimmed_from_the_source_do_not_shrink_the_visible_line() {
        let mut buffer = CaptionLineBuffer::default();

        assert_eq!(
            caption(&mut buffer, "one two three four", 2, 100),
            "one two three four"
        );
        assert_eq!(
            caption(&mut buffer, "two three four five", 2, 100),
            "one two three four five"
        );
    }

    #[test]
    fn a_shorter_aligned_partial_keeps_the_visible_words() {
        let mut buffer = CaptionLineBuffer::default();

        assert_eq!(
            caption(&mut buffer, "one two three four", 2, 100),
            "one two three four"
        );
        assert_eq!(
            caption(&mut buffer, "one two three", 2, 100),
            "one two three four"
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
