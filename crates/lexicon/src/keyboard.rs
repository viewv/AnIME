//! QWERTY 物理键盘几何距离矩阵与加权 Damerau-Levenshtein 代价模型。

use std::sync::OnceLock;

pub const TRANSPOSITION_COST: f32 = 0.45;
pub const DOUBLE_CHAR_COST: f32 = 0.40;
pub const APOSTROPHE_SKIP_COST: f32 = 0.15;

// 标准美式 QWERTY 物理按键交错几何坐标：
// 第二行向右偏移 0.25 键宽，第三行向右偏移 0.75 键宽，以真实刻画手指横纵斜向误触概率
// （例如 'i' (7.0, 0.0) 与 'l' (8.25, 1.0) 欧氏距离 1.60，显著小于远端按键误触代价）。
const fn key_coords(ch: u8) -> Option<(f32, f32)> {
    match ch.to_ascii_lowercase() {
        b'q' => Some((0.0, 0.0)),
        b'w' => Some((1.0, 0.0)),
        b'e' => Some((2.0, 0.0)),
        b'r' => Some((3.0, 0.0)),
        b't' => Some((4.0, 0.0)),
        b'y' => Some((5.0, 0.0)),
        b'u' => Some((6.0, 0.0)),
        b'i' => Some((7.0, 0.0)),
        b'o' => Some((8.0, 0.0)),
        b'p' => Some((9.0, 0.0)),

        b'a' => Some((0.25, 1.0)),
        b's' => Some((1.25, 1.0)),
        b'd' => Some((2.25, 1.0)),
        b'f' => Some((3.25, 1.0)),
        b'g' => Some((4.25, 1.0)),
        b'h' => Some((5.25, 1.0)),
        b'j' => Some((6.25, 1.0)),
        b'k' => Some((7.25, 1.0)),
        b'l' => Some((8.25, 1.0)),

        b'z' => Some((0.75, 2.0)),
        b'x' => Some((1.75, 2.0)),
        b'c' => Some((2.75, 2.0)),
        b'v' => Some((3.75, 2.0)),
        b'b' => Some((4.75, 2.0)),
        b'n' => Some((5.75, 2.0)),
        b'm' => Some((6.75, 2.0)),
        _ => None,
    }
}

#[inline]
const fn is_vowel_or_y(ch: u8) -> bool {
    matches!(
        ch.to_ascii_lowercase(),
        b'a' | b'e' | b'i' | b'o' | b'u' | b'y'
    )
}

#[inline]
const fn is_soft_silent_or_vowel(ch: u8) -> bool {
    matches!(
        ch.to_ascii_lowercase(),
        b'a' | b'e' | b'i' | b'o' | b'u' | b'h' | b'n' | b'd' | b't'
    )
}

fn euclidean_dist(a: u8, b: u8) -> Option<f32> {
    let (ax, ay) = key_coords(a)?;
    let (bx, by) = key_coords(b)?;
    let dx = ax - bx;
    let dy = ay - by;
    Some((dx * dx + dy * dy).sqrt())
}

fn sub_cost_table() -> &'static [[f32; 26]; 26] {
    static TABLE: OnceLock<[[f32; 26]; 26]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut mat = [[1.05_f32; 26]; 26];
        for i in 0..26 {
            let ca = b'a' + i as u8;
            for j in 0..26 {
                let cb = b'a' + j as u8;
                if i == j {
                    mat[i][j] = 0.0;
                    continue;
                }
                let mut cost = match euclidean_dist(ca, cb) {
                    Some(d) if d <= 1.15 => 0.45,
                    Some(d) if d <= 1.65 => 0.62,
                    Some(d) if d <= 2.35 => 0.85,
                    _ => 1.05,
                };
                // 元音间混淆（如 seperate / definately）在英文拼写中极常见，封顶为 0.70
                if is_vowel_or_y(ca) && is_vowel_or_y(cb) && cost > 0.70 {
                    cost = 0.70;
                }
                mat[i][j] = cost;
            }
        }
        mat
    })
}

#[inline]
pub fn is_direct_neighbor(a: u8, b: u8) -> bool {
    let al = a.to_ascii_lowercase();
    let bl = b.to_ascii_lowercase();
    if al == bl {
        return true;
    }
    matches!(euclidean_dist(al, bl), Some(d) if d <= 1.25)
}

/// 返回敲错键 `typed` 替换为真实键 `target` 的物理/语音混合代价（O(1) 查表）。
#[inline]
pub fn substitution_cost(typed: u8, target: u8) -> f32 {
    let tl = typed.to_ascii_lowercase();
    let gl = target.to_ascii_lowercase();
    if tl == gl {
        return 0.0;
    }
    if tl.is_ascii_lowercase() && gl.is_ascii_lowercase() {
        return sub_cost_table()[usize::from(tl - b'a')][usize::from(gl - b'a')];
    }
    1.0
}

/// 漏敲真实字符 `omitted` 的代价：叠字漏敲与元音/弱哑音漏敲显著低于实义辅音漏敲。
#[inline]
pub fn deletion_cost(omitted: u8, prev_target: Option<u8>) -> f32 {
    let ol = omitted.to_ascii_lowercase();
    if ol == b'\'' {
        return APOSTROPHE_SKIP_COST;
    }
    if let Some(prev) = prev_target {
        if prev.to_ascii_lowercase() == ol {
            return DOUBLE_CHAR_COST;
        }
    }
    if is_soft_silent_or_vowel(ol) {
        0.65
    } else {
        0.80
    }
}

/// 多敲键 `extra` 的代价：连击重复或擦碰到相邻键的额外按键代价更低。
#[inline]
pub fn insertion_cost(extra: u8, prev_typed: Option<u8>, next_typed: Option<u8>) -> f32 {
    let el = extra.to_ascii_lowercase();
    if let Some(prev) = prev_typed {
        let pl = prev.to_ascii_lowercase();
        if pl == el {
            return DOUBLE_CHAR_COST;
        }
        if is_direct_neighbor(pl, el) {
            return 0.55;
        }
    }
    if let Some(next) = next_typed {
        if is_direct_neighbor(next, el) {
            return 0.55;
        }
    }
    0.80
}

/// 计算输入串 `typed` 与候选词 `candidate` 之间的加权 Damerau-Levenshtein 编辑代价。
pub fn weighted_damerau_levenshtein(typed: &str, candidate: &str) -> f32 {
    let a: Vec<u8> = typed.bytes().map(|b| b.to_ascii_lowercase()).collect();
    let b: Vec<u8> = candidate.bytes().map(|b| b.to_ascii_lowercase()).collect();
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return (0..m)
            .map(|j| deletion_cost(b[j], if j > 0 { Some(b[j - 1]) } else { None }))
            .sum();
    }
    if m == 0 {
        return (0..n)
            .map(|i| {
                insertion_cost(
                    a[i],
                    if i > 0 { Some(a[i - 1]) } else { None },
                    a.get(i + 1).copied(),
                )
            })
            .sum();
    }

    let cols = m + 1;
    let mut dp = vec![f32::INFINITY; (n + 1) * cols];
    dp[0] = 0.0;
    for i in 1..=n {
        let prev = if i >= 2 { Some(a[i - 2]) } else { None };
        let next = a.get(i).copied();
        dp[i * cols] = dp[(i - 1) * cols] + insertion_cost(a[i - 1], prev, next);
    }
    for j in 1..=m {
        let prev = if j >= 2 { Some(b[j - 2]) } else { None };
        dp[j] = dp[j - 1] + deletion_cost(b[j - 1], prev);
    }

    for i in 1..=n {
        let prev_a = if i >= 2 { Some(a[i - 2]) } else { None };
        let next_a = a.get(i).copied();
        for j in 1..=m {
            let prev_b = if j >= 2 { Some(b[j - 2]) } else { None };
            let sub = if a[i - 1] == b[j - 1] {
                0.0
            } else {
                substitution_cost(a[i - 1], b[j - 1])
            };
            let mut best = dp[(i - 1) * cols + (j - 1)] + sub;
            let del = dp[i * cols + (j - 1)] + deletion_cost(b[j - 1], prev_b);
            if del < best {
                best = del;
            }
            let ins = dp[(i - 1) * cols + j] + insertion_cost(a[i - 1], prev_a, next_a);
            if ins < best {
                best = ins;
            }
            if i >= 2 && j >= 2 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] && a[i - 1] != a[i - 2] {
                let trans = dp[(i - 2) * cols + (j - 2)] + TRANSPOSITION_COST;
                if trans < best {
                    best = trans;
                }
            }
            dp[i * cols + j] = best;
        }
    }

    dp[n * cols + m]
}
