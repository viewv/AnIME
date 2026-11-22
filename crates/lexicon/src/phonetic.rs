//! 纯 Rust Double Metaphone 读音编码与语音倒排索引（解决重度读音拼错与哑字母纠错）。

use std::collections::{HashMap, HashSet};

const MAX_PHONETIC_LEN: usize = 6;
const MAX_PREFIX4_BUCKET: usize = 64;

#[derive(Default)]
struct PhoneticBuf {
    primary: String,
    secondary: String,
    has_alt: bool,
}

impl PhoneticBuf {
    fn push_both(&mut self, ch: char) {
        if self.primary.len() < MAX_PHONETIC_LEN {
            self.primary.push(ch);
        }
        if self.secondary.len() < MAX_PHONETIC_LEN {
            self.secondary.push(ch);
        }
    }

    fn push_both_str(&mut self, s: &str) {
        for ch in s.chars() {
            self.push_both(ch);
        }
    }

    fn push_alt(&mut self, pri: &str, sec: &str) {
        if pri != sec {
            self.has_alt = true;
        }
        for ch in pri.chars() {
            if self.primary.len() < MAX_PHONETIC_LEN {
                self.primary.push(ch);
            }
        }
        for ch in sec.chars() {
            if self.secondary.len() < MAX_PHONETIC_LEN {
                self.secondary.push(ch);
            }
        }
    }

    fn done(&self) -> bool {
        self.primary.len() >= MAX_PHONETIC_LEN && self.secondary.len() >= MAX_PHONETIC_LEN
    }

    fn finish(self) -> (String, Option<String>) {
        let alt = if self.has_alt && self.secondary != self.primary {
            Some(self.secondary)
        } else {
            None
        };
        (self.primary, alt)
    }
}

#[inline]
fn is_vowel(b: u8) -> bool {
    matches!(b, b'A' | b'E' | b'I' | b'O' | b'U' | b'Y')
}

#[inline]
fn byte_at(bytes: &[u8], idx: isize) -> u8 {
    if idx < 0 || (idx as usize) >= bytes.len() {
        b' '
    } else {
        bytes[idx as usize]
    }
}

fn slice_eq(bytes: &[u8], start: isize, len: usize, opts: &[&str]) -> bool {
    if start < 0 {
        return false;
    }
    let s = start as usize;
    let Some(end) = s.checked_add(len) else {
        return false;
    };
    if end > bytes.len() {
        return false;
    }
    let seg = &bytes[s..end];
    opts.iter().any(|opt| opt.as_bytes() == seg)
}

fn is_slavo_germanic(bytes: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(bytes) else {
        return false;
    };
    s.contains('W') || s.contains('K') || s.contains("CZ") || s.contains("WITZ")
}

/// 计算英文单词的 Double Metaphone 主副读音编码（最长 6 位 ASCII 大写符号）。
pub fn encode_double_metaphone(raw: &str) -> (String, Option<String>) {
    let upper: Vec<u8> = raw
        .bytes()
        .filter(|b| b.is_ascii_alphabetic())
        .map(|b| b.to_ascii_uppercase())
        .collect();
    if upper.is_empty() {
        return (String::new(), None);
    }

    let len = upper.len() as isize;
    let slavo_germanic = is_slavo_germanic(&upper);
    let mut buf = PhoneticBuf::default();
    let mut cur: isize = 0;

    // 英文词首哑辅音组合（KN/GN/PN/WR/PS）直接跳过首字母，
    // 使 "pneumonia"/"numonia"、"psychology"/"sikology"、"knowledge"/"nolege" 落入同一语音桶。
    if slice_eq(&upper, 0, 2, &["GN", "KN", "PN", "WR", "PS"]) {
        cur = 1;
    }
    if byte_at(&upper, 0) == b'X' {
        buf.push_both('S');
        cur = 1;
    }

    while cur < len && !buf.done() {
        let ch = byte_at(&upper, cur);
        match ch {
            b'A' | b'E' | b'I' | b'O' | b'U' | b'Y' => {
                if cur == 0 {
                    buf.push_both('A');
                }
                cur += 1;
            }
            b'B' => {
                // 词尾 -MB（plumber/climb）与 BT 前的哑音 B（subtle/debt/doubt）
                let silent_bt = byte_at(&upper, cur + 1) == b'T';
                if silent_bt {
                    buf.push_alt("", "P");
                } else {
                    buf.push_both('P');
                }
                cur += if byte_at(&upper, cur + 1) == b'B' { 2 } else { 1 };
            }
            b'C' => {
                if cur > 1
                    && !is_vowel(byte_at(&upper, cur - 2))
                    && slice_eq(&upper, cur - 1, 3, &["ACH"])
                    && byte_at(&upper, cur + 2) != b'I'
                    && (byte_at(&upper, cur + 2) != b'E'
                        || slice_eq(&upper, cur - 2, 6, &["BACHER", "MACHER"]))
                {
                    buf.push_both('K');
                    cur += 2;
                } else if cur == 0 && slice_eq(&upper, 0, 6, &["CAESAR"]) {
                    buf.push_both('S');
                    cur += 2;
                } else if slice_eq(&upper, cur, 4, &["CHIA"]) {
                    buf.push_both('K');
                    cur += 2;
                } else if slice_eq(&upper, cur, 2, &["CH"]) {
                    if cur > 0 && slice_eq(&upper, cur, 4, &["CHAE"]) {
                        buf.push_alt("K", "X");
                        cur += 2;
                    } else if cur == 0
                        && (slice_eq(&upper, cur + 1, 5, &["HARAC", "HARIS"])
                            || slice_eq(&upper, cur + 1, 3, &["HOR", "HYM", "HIA", "HEM"]))
                        && !slice_eq(&upper, 0, 5, &["CHORE"])
                    {
                        buf.push_both('K');
                        cur += 2;
                    } else if slice_eq(&upper, 0, 4, &["VAN ", "VON "])
                        || slice_eq(&upper, 0, 3, &["SCH"])
                        || slice_eq(
                            &upper,
                            cur - 2,
                            6,
                            &["ORCHES", "ARCHIT", "ORCHID"],
                        )
                        || slice_eq(&upper, cur - 3, 5, &["PSYCH"])
                        || slice_eq(&upper, cur - 2, 4, &["SYCH"])
                        || slice_eq(&upper, cur + 2, 1, &["T", "S"])
                        || ((cur == 0
                            || slice_eq(&upper, cur - 1, 1, &["A", "O", "U", "E"]))
                            && slice_eq(
                                &upper,
                                cur + 2,
                                1,
                                &["L", "R", "N", "M", "B", "H", "F", "V", "W", " "],
                            ))
                        || slice_eq(&upper, cur, 3, &["CHR"])
                        || slice_eq(&upper, cur - 5, 7, &["STOMACH"])
                    {
                        // Greek/Germanic hard CH（psychology/chronology/schedule/stomach）读 /k/
                        buf.push_both('K');
                        cur += 2;
                    } else if cur > 0 {
                        if slice_eq(&upper, 0, 2, &["MC"]) {
                            buf.push_both('K');
                        } else {
                            buf.push_alt("X", "K");
                        }
                        cur += 2;
                    } else {
                        buf.push_both('X');
                        cur += 2;
                    }
                } else if slice_eq(&upper, cur, 2, &["CZ"]) && !slice_eq(&upper, cur - 2, 4, &["WICZ"]) {
                    buf.push_alt("S", "X");
                    cur += 2;
                } else if slice_eq(&upper, cur + 1, 3, &["CIA"]) {
                    buf.push_both('X');
                    cur += 3;
                } else if slice_eq(&upper, cur, 2, &["CC"])
                    && !(cur == 1 && byte_at(&upper, 0) == b'M')
                {
                    if slice_eq(&upper, cur + 2, 1, &["I", "E", "H"])
                        && !slice_eq(&upper, cur + 2, 2, &["HU"])
                    {
                        if (cur == 1 && byte_at(&upper, 0) == b'A')
                            || slice_eq(&upper, cur - 1, 5, &["UCCEE", "UCCES"])
                        {
                            buf.push_both_str("KS");
                        } else {
                            buf.push_both('X');
                        }
                        cur += 3;
                    } else {
                        buf.push_both('K');
                        cur += 2;
                    }
                } else if slice_eq(&upper, cur, 2, &["CK", "CG", "CQ"]) {
                    buf.push_both('K');
                    cur += 2;
                } else if slice_eq(&upper, cur, 2, &["CI", "CE", "CY"]) {
                    if slice_eq(&upper, cur, 3, &["CIO", "CIE", "CIA"]) {
                        buf.push_alt("S", "X");
                    } else {
                        buf.push_both('S');
                    }
                    cur += 2;
                } else {
                    buf.push_both('K');
                    if slice_eq(&upper, cur + 1, 2, &[" C", " Q", " G"]) {
                        cur += 3;
                    } else if slice_eq(&upper, cur + 1, 1, &["C", "K", "Q"])
                        && !slice_eq(&upper, cur + 1, 2, &["CE", "CI"])
                    {
                        cur += 2;
                    } else {
                        cur += 1;
                    }
                }
            }
            b'D' => {
                if slice_eq(&upper, cur, 2, &["DG"]) {
                    if slice_eq(&upper, cur + 2, 1, &["I", "E", "Y"]) {
                        buf.push_both('J');
                        cur += 3;
                    } else {
                        buf.push_both_str("TK");
                        cur += 2;
                    }
                } else if slice_eq(&upper, cur, 2, &["DT", "DD"]) {
                    buf.push_both('T');
                    cur += 2;
                } else if byte_at(&upper, cur + 1) == b'N' {
                    // 覆盖 "Wednesday" 中 DN 的口语吞音 D
                    buf.push_alt("T", "");
                    cur += 1;
                } else {
                    buf.push_both('T');
                    cur += 1;
                }
            }
            b'F' => {
                buf.push_both('F');
                cur += if byte_at(&upper, cur + 1) == b'F' { 2 } else { 1 };
            }
            b'G' => {
                if byte_at(&upper, cur + 1) == b'H' {
                    if cur > 0 && !is_vowel(byte_at(&upper, cur - 1)) {
                        buf.push_both('K');
                        cur += 2;
                    } else if cur == 0 {
                        if byte_at(&upper, cur + 2) == b'I' {
                            buf.push_both('J');
                        } else {
                            buf.push_both('K');
                        }
                        cur += 2;
                    } else if (cur > 1 && slice_eq(&upper, cur - 2, 1, &["B", "H", "D"]))
                        || (cur > 2 && slice_eq(&upper, cur - 3, 1, &["B", "H", "D"]))
                        || (cur > 3 && slice_eq(&upper, cur - 4, 1, &["B", "H"]))
                    {
                        cur += 2;
                    } else {
                        // -OUGH / -AUGH 在 C/G/L/R/T/N 后常发 /f/（enough/laugh/tough/rough/cough），
                        // 其余 -OUGH/-AUGH（through/though/caught）按英语常见习惯静音处理
                        if cur > 2
                            && byte_at(&upper, cur - 1) == b'U'
                            && slice_eq(&upper, cur - 3, 1, &["C", "G", "L", "R", "T", "N"])
                        {
                            buf.push_both('F');
                        } else if cur > 0
                            && byte_at(&upper, cur - 1) != b'I'
                            && !slice_eq(&upper, cur - 2, 4, &["OUGH", "AUGH"])
                        {
                            buf.push_both('K');
                        }
                        cur += 2;
                    }
                } else if byte_at(&upper, cur + 1) == b'N' {
                    if cur == 1 && is_vowel(byte_at(&upper, 0)) && !slavo_germanic {
                        buf.push_alt("KN", "N");
                    } else if !slice_eq(&upper, cur + 2, 2, &["EY"])
                        && byte_at(&upper, cur + 1) != b'Y'
                        && !slavo_germanic
                    {
                        buf.push_alt("N", "KN");
                    } else {
                        buf.push_both_str("KN");
                    }
                    cur += 2;
                } else if slice_eq(&upper, cur + 1, 2, &["LI"]) && !slavo_germanic {
                    buf.push_alt("KL", "L");
                    cur += 2;
                } else if cur == 0
                    && (byte_at(&upper, cur + 1) == b'Y'
                        || slice_eq(
                            &upper,
                            cur + 1,
                            2,
                            &["ES", "EP", "EB", "EL", "EY", "IB", "IL", "IN", "IE", "EI", "ER"],
                        ))
                {
                    buf.push_alt("K", "J");
                    cur += 2;
                } else if (slice_eq(&upper, cur + 1, 2, &["ER"]) || byte_at(&upper, cur + 1) == b'Y')
                    && !slice_eq(&upper, 0, 6, &["DANGER", "RANGER", "MANGER"])
                    && !slice_eq(&upper, cur - 1, 1, &["E", "I"])
                    && !slice_eq(&upper, cur - 1, 3, &["RGY", "OGY"])
                {
                    buf.push_alt("K", "J");
                    cur += 2;
                } else if slice_eq(&upper, cur + 1, 1, &["E", "I", "Y"])
                    || slice_eq(&upper, cur - 1, 4, &["AGGI", "OGGI"])
                {
                    if slice_eq(&upper, 0, 4, &["VAN ", "VON "])
                        || slice_eq(&upper, 0, 3, &["SCH"])
                        || slice_eq(&upper, cur + 1, 2, &["ET"])
                    {
                        buf.push_both('K');
                    } else if slice_eq(&upper, cur + 1, 4, &["IER "]) {
                        buf.push_both('J');
                    } else {
                        buf.push_alt("J", "K");
                    }
                    cur += if byte_at(&upper, cur + 1) == b'G' { 2 } else { 1 };
                } else {
                    buf.push_both('K');
                    cur += if byte_at(&upper, cur + 1) == b'G' { 2 } else { 1 };
                }
            }
            b'H' => {
                // H 仅在词首元音前或两元音之间才发音
                if (cur == 0 || is_vowel(byte_at(&upper, cur - 1)))
                    && is_vowel(byte_at(&upper, cur + 1))
                {
                    buf.push_both('H');
                    cur += 2;
                } else {
                    cur += 1;
                }
            }
            b'J' => {
                if slice_eq(&upper, cur, 4, &["JOSE"]) || slice_eq(&upper, 0, 4, &["SAN "]) {
                    if (cur == 0 && byte_at(&upper, cur + 4) == b' ')
                        || slice_eq(&upper, 0, 4, &["SAN "])
                    {
                        buf.push_both('H');
                    } else {
                        buf.push_alt("J", "H");
                    }
                } else if cur == 0 && !slice_eq(&upper, cur, 4, &["JOSE"]) {
                    buf.push_alt("J", "A");
                } else if is_vowel(byte_at(&upper, cur - 1))
                    && !slavo_germanic
                    && slice_eq(&upper, cur + 1, 1, &["A", "O"])
                {
                    buf.push_alt("J", "H");
                } else if cur == len - 1 {
                    buf.push_alt("J", " ");
                } else if !slice_eq(
                    &upper,
                    cur + 1,
                    1,
                    &["L", "T", "K", "S", "N", "M", "B", "Z"],
                ) && !slice_eq(&upper, cur - 1, 1, &["S", "K", "L"])
                {
                    buf.push_both('J');
                }
                cur += if byte_at(&upper, cur + 1) == b'J' { 2 } else { 1 };
            }
            b'K' => {
                buf.push_both('K');
                cur += if byte_at(&upper, cur + 1) == b'K' { 2 } else { 1 };
            }
            b'L' => {
                // 英语中 -OLONEL（colonel）读音近 /kurnel/，保留次级 R 映射便于与 kurnel 匹配
                if byte_at(&upper, cur + 1) == b'L' {
                    if (cur == len - 3
                        && slice_eq(&upper, cur - 1, 4, &["ILLO", "ILLA", "ALLE"]))
                        || ((slice_eq(&upper, len - 2, 2, &["AS", "OS"])
                            || slice_eq(&upper, len - 1, 1, &["A", "O"]))
                            && slice_eq(&upper, cur - 1, 4, &["ALLE"]))
                    {
                        buf.push_alt("L", "");
                        cur += 2;
                    } else {
                        buf.push_both('L');
                        cur += 2;
                    }
                } else {
                    buf.push_both('L');
                    cur += 1;
                }
            }
            b'M' => {
                buf.push_both('M');
                if slice_eq(&upper, cur - 1, 3, &["UMB"])
                    && (cur + 1 == len - 1 || slice_eq(&upper, cur + 2, 2, &["ER"]))
                {
                    cur += 2;
                } else {
                    cur += if byte_at(&upper, cur + 1) == b'M' { 2 } else { 1 };
                }
            }
            b'N' => {
                buf.push_both('N');
                cur += if byte_at(&upper, cur + 1) == b'N' { 2 } else { 1 };
            }
            b'P' => {
                if byte_at(&upper, cur + 1) == b'H' {
                    buf.push_both('F');
                    cur += 2;
                } else if slice_eq(&upper, cur - 2, 4, &["EIPT", "EMPT"]) {
                    // 覆盖 receipt 中的词尾哑音 P
                    buf.push_alt("", "P");
                    cur += 1;
                } else {
                    buf.push_both('P');
                    cur += if slice_eq(&upper, cur + 1, 1, &["P", "B"]) {
                        2
                    } else {
                        1
                    };
                }
            }
            b'Q' => {
                buf.push_both('K');
                cur += if byte_at(&upper, cur + 1) == b'Q' { 2 } else { 1 };
            }
            b'R' => {
                if cur == len - 1
                    && !slavo_germanic
                    && slice_eq(&upper, cur - 2, 2, &["IE"])
                    && !slice_eq(&upper, cur - 4, 2, &["ME", "MA"])
                {
                    buf.push_alt("", "R");
                } else {
                    buf.push_both('R');
                }
                cur += if byte_at(&upper, cur + 1) == b'R' { 2 } else { 1 };
            }
            b'S' => {
                if slice_eq(&upper, cur - 1, 3, &["ISL", "YSL"]) {
                    cur += 1;
                } else if cur == 0 && slice_eq(&upper, 0, 5, &["SUGAR"]) {
                    buf.push_alt("X", "S");
                    cur += 1;
                } else if slice_eq(&upper, cur, 2, &["SH"]) {
                    if slice_eq(&upper, cur + 1, 4, &["HEIM", "HOEK", "HOLM", "HOLZ"]) {
                        buf.push_both('S');
                    } else {
                        buf.push_both('X');
                    }
                    cur += 2;
                } else if slice_eq(&upper, cur, 3, &["SIO", "SIA"])
                    || slice_eq(&upper, cur, 4, &["SIAN"])
                {
                    if !slavo_germanic {
                        buf.push_alt("S", "X");
                    } else {
                        buf.push_both('S');
                    }
                    cur += 3;
                } else if (cur == 0 && slice_eq(&upper, cur + 1, 1, &["M", "N", "L", "W"]))
                    || byte_at(&upper, cur + 1) == b'Z'
                {
                    buf.push_alt("S", "X");
                    cur += if byte_at(&upper, cur + 1) == b'Z' { 2 } else { 1 };
                } else if slice_eq(&upper, cur, 2, &["SC"]) {
                    if byte_at(&upper, cur + 2) == b'H' {
                        if slice_eq(
                            &upper,
                            cur + 3,
                            2,
                            &["OO", "ER", "EN", "UY", "ED", "EM"],
                        ) {
                            if slice_eq(&upper, cur + 3, 2, &["ER", "EN"]) {
                                buf.push_alt("X", "SK");
                            } else {
                                buf.push_both_str("SK");
                            }
                            cur += 3;
                        } else if cur == 0 && !is_vowel(byte_at(&upper, 3)) && byte_at(&upper, 3) != b'W' {
                            buf.push_alt("X", "S");
                            cur += 3;
                        } else {
                            // 如 schedule -> skedule / shedule
                            buf.push_alt("X", "SK");
                            cur += 3;
                        }
                    } else if slice_eq(&upper, cur + 2, 1, &["I", "E", "Y"]) {
                        if slice_eq(&upper, cur + 3, 2, &["OU", "EN"]) {
                            // 如 conscious/conscience -> X
                            buf.push_alt("X", "S");
                        } else {
                            buf.push_both('S');
                        }
                        cur += 3;
                    } else {
                        buf.push_both_str("SK");
                        cur += 3;
                    }
                } else {
                    if cur == len - 1 && slice_eq(&upper, cur - 2, 2, &["AI", "OI"]) {
                        buf.push_alt("", "S");
                    } else {
                        buf.push_both('S');
                    }
                    cur += if slice_eq(&upper, cur + 1, 1, &["S", "Z"]) {
                        2
                    } else {
                        1
                    };
                }
            }
            b'T' => {
                if slice_eq(&upper, cur, 4, &["TION"])
                    || slice_eq(&upper, cur, 3, &["TIA", "TCH"])
                {
                    buf.push_both('X');
                    cur += 3;
                } else if slice_eq(&upper, cur, 2, &["TH"])
                    || slice_eq(&upper, cur, 3, &["TTH"])
                {
                    if slice_eq(&upper, cur + 2, 2, &["OM", "AM"])
                        || slice_eq(&upper, 0, 4, &["VAN ", "VON "])
                        || slice_eq(&upper, 0, 3, &["SCH"])
                    {
                        buf.push_both('T');
                    } else {
                        buf.push_alt("0", "T");
                    }
                    cur += 2;
                } else {
                    buf.push_both('T');
                    cur += if slice_eq(&upper, cur + 1, 1, &["T", "D"]) {
                        2
                    } else {
                        1
                    };
                }
            }
            b'V' => {
                buf.push_both('F');
                cur += if byte_at(&upper, cur + 1) == b'V' { 2 } else { 1 };
            }
            b'W' => {
                if slice_eq(&upper, cur, 2, &["WR"]) {
                    buf.push_both('R');
                    cur += 2;
                } else if cur == 0
                    && (is_vowel(byte_at(&upper, cur + 1))
                        || slice_eq(&upper, cur, 2, &["WH"]))
                {
                    if is_vowel(byte_at(&upper, cur + 1)) {
                        buf.push_alt("A", "F");
                    } else {
                        buf.push_both('A');
                    }
                    cur += 1;
                } else if (cur == len - 1 && is_vowel(byte_at(&upper, cur - 1)))
                    || slice_eq(
                        &upper,
                        cur - 1,
                        5,
                        &["EWSKI", "EWSKY", "OWSKI", "OWSKY"],
                    )
                    || slice_eq(&upper, 0, 3, &["SCH"])
                {
                    buf.push_alt("", "F");
                    cur += 1;
                } else if slice_eq(&upper, cur, 4, &["WICZ", "WITZ"]) {
                    buf.push_alt("TS", "FX");
                    cur += 4;
                } else {
                    cur += 1;
                }
            }
            b'X' => {
                if !(cur == len - 1
                    && (slice_eq(&upper, cur - 3, 3, &["IAU", "EAU"])
                        || slice_eq(&upper, cur - 2, 2, &["AU", "OU"])))
                {
                    buf.push_both_str("KS");
                }
                cur += if slice_eq(&upper, cur + 1, 1, &["C", "X"]) {
                    2
                } else {
                    1
                };
            }
            b'Z' => {
                if byte_at(&upper, cur + 1) == b'H' {
                    buf.push_both('J');
                    cur += 2;
                } else if slice_eq(&upper, cur + 1, 2, &["ZO", "ZI", "ZA"])
                    || (slavo_germanic && cur > 0 && byte_at(&upper, cur - 1) != b'T')
                {
                    buf.push_alt("S", "TS");
                    cur += if byte_at(&upper, cur + 1) == b'Z' { 2 } else { 1 };
                } else {
                    buf.push_both('S');
                    cur += if byte_at(&upper, cur + 1) == b'Z' { 2 } else { 1 };
                }
            }
            _ => {
                cur += 1;
            }
        }
    }

    buf.finish()
}

/// 基于 Double Metaphone 的词级语音倒排索引：按词频降序保存各读音桶的高优词 ID。
#[derive(Debug, Clone, Default)]
pub struct PhoneticIndex {
    exact: HashMap<String, Vec<u32>>,
    prefix4: HashMap<String, Vec<u32>>,
}

impl PhoneticIndex {
    pub fn build<'a, I>(words_by_freq: I) -> Self
    where
        I: IntoIterator<Item = (u32, &'a str)>,
    {
        // 要求调用侧按词频从高到低传入，或随后每个桶自动维持高词频在前且去重
        let mut exact: HashMap<String, Vec<u32>> = HashMap::new();
        let mut prefix4: HashMap<String, Vec<u32>> = HashMap::new();

        for (word_id, word) in words_by_freq {
            let (pri, sec) = encode_double_metaphone(word);
            for code in [Some(pri), sec].into_iter().flatten() {
                if code.is_empty() {
                    continue;
                }
                let bucket = exact.entry(code.clone()).or_default();
                if bucket.last().copied() != Some(word_id) && !bucket.contains(&word_id) {
                    bucket.push(word_id);
                }
                if code.len() > 4 {
                    let p4 = &code[..4];
                    let pb = prefix4.entry(p4.to_string()).or_default();
                    if pb.len() < MAX_PREFIX4_BUCKET
                        && pb.last().copied() != Some(word_id)
                        && !pb.contains(&word_id)
                    {
                        pb.push(word_id);
                    }
                }
            }
        }

        Self { exact, prefix4 }
    }

    /// 检索读音相近候选词 ID：`match_tier` 0=精确音码，1=音码单步换位/漏码，2=4位音码前缀。
    pub fn lookup(&self, raw: &str, max_candidates: usize) -> Vec<(u32, u8)> {
        let (pri, sec) = encode_double_metaphone(raw);
        let mut seen: HashSet<u32> = HashSet::new();
        let mut out: Vec<(u32, u8)> = Vec::new();

        // 1. 精确 Double Metaphone 主副音码桶
        for code in [Some(pri.as_str()), sec.as_deref()].into_iter().flatten() {
            if code.is_empty() {
                continue;
            }
            if let Some(ids) = self.exact.get(code) {
                for &id in ids.iter().take(max_candidates) {
                    if seen.insert(id) {
                        out.push((id, 0));
                        if out.len() >= max_candidates {
                            return out;
                        }
                    }
                }
            }
        }

        // 2. 对短音码做单步邻位换位探测（仅在精确音码召回不足时触发）
        if out.len() < 8 && (3..=MAX_PHONETIC_LEN).contains(&pri.len()) {
            let b = pri.as_bytes();
            for i in 0..b.len().saturating_sub(1) {
                if b[i] == b[i + 1] {
                    continue;
                }
                let mut swapped = b.to_vec();
                swapped.swap(i, i + 1);
                if let Ok(key) = std::str::from_utf8(&swapped) {
                    if let Some(ids) = self.exact.get(key) {
                        for &id in ids.iter().take(8) {
                            if seen.insert(id) {
                                out.push((id, 1));
                                if out.len() >= max_candidates {
                                    return out;
                                }
                            }
                        }
                    }
                }
            }
        }

        // 3. 长词 4 位语音前缀桶（兜底多音节重度误拼尾部偏差）
        if out.len() < max_candidates {
            for code in [Some(pri.as_str()), sec.as_deref()].into_iter().flatten() {
                if code.len() >= 4 {
                    if let Some(ids) = self.prefix4.get(&code[..4]) {
                        for &id in ids.iter().take(12) {
                            if seen.insert(id) {
                                out.push((id, 2));
                                if out.len() >= max_candidates {
                                    return out;
                                }
                            }
                        }
                    }
                }
            }
        }

        out
    }
}
