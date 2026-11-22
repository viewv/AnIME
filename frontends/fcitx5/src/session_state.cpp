#include "session_state.h"

#include <algorithm>

namespace anime {
namespace {

bool decodeOneUtf8(std::string_view s, size_t pos, size_t &step, uint32_t &cp) {
    if (pos >= s.size()) {
        step = 0;
        return false;
    }
    const auto b0 = static_cast<unsigned char>(s[pos]);
    if ((b0 & 0x80U) == 0) {
        step = 1;
        cp = b0;
        return true;
    }
    size_t len = 0;
    uint32_t acc = 0;
    uint32_t min_cp = 0;
    if ((b0 & 0xE0U) == 0xC0U) {
        len = 2;
        acc = b0 & 0x1FU;
        min_cp = 0x80U;
    } else if ((b0 & 0xF0U) == 0xE0U) {
        len = 3;
        acc = b0 & 0x0FU;
        min_cp = 0x800U;
    } else if ((b0 & 0xF8U) == 0xF0U) {
        len = 4;
        acc = b0 & 0x07U;
        min_cp = 0x10000U;
    } else {
        step = 1;
        return false;
    }
    if (pos + len > s.size()) {
        step = 1;
        return false;
    }
    for (size_t i = 1; i < len; ++i) {
        const auto bi = static_cast<unsigned char>(s[pos + i]);
        if ((bi & 0xC0U) != 0x80U) {
            step = 1;
            return false;
        }
        acc = (acc << 6U) | (bi & 0x3FU);
    }
    if (acc < min_cp || acc > 0x10FFFFU || (acc >= 0xD800U && acc <= 0xDFFFU)) {
        step = 1;
        return false;
    }
    step = len;
    cp = acc;
    return true;
}

size_t countUtf8Chars(std::string_view s) {
    size_t count = 0;
    size_t pos = 0;
    while (pos < s.size()) {
        size_t step = 0;
        uint32_t cp = 0;
        decodeOneUtf8(s, pos, step, cp);
        pos += (step > 0 ? step : 1);
        ++count;
    }
    return count;
}

std::string skipFirstNCharsAndTrimLeadingSpaces(std::string_view s, size_t skip_chars) {
    const size_t byte_off = utf8CharOffsetToByteOffset(s, skip_chars);
    std::string_view tail = s.substr(byte_off);
    while (!tail.empty() && tail.front() == ' ') {
        tail.remove_prefix(1);
    }
    return std::string(tail);
}

void popLastUtf8Codepoint(std::string &s) {
    while (!s.empty()) {
        const auto back = static_cast<unsigned char>(s.back());
        s.pop_back();
        if ((back & 0xC0U) != 0x80U) {
            break;
        }
    }
}

std::string_view trimAsciiSpaces(std::string_view s) {
    while (!s.empty() && (s.front() == ' ' || s.front() == '\t')) {
        s.remove_prefix(1);
    }
    while (!s.empty() && (s.back() == ' ' || s.back() == '\t')) {
        s.remove_suffix(1);
    }
    return s;
}

unsigned char toAsciiLower(unsigned char c) {
    return (c >= 'A' && c <= 'Z') ? static_cast<unsigned char>(c + ('a' - 'A')) : c;
}

bool asciiEqualsIgnoreCase(std::string_view a, std::string_view b) {
    if (a.size() != b.size()) {
        return false;
    }
    for (size_t i = 0; i < a.size(); ++i) {
        if (toAsciiLower(static_cast<unsigned char>(a[i])) !=
            toAsciiLower(static_cast<unsigned char>(b[i]))) {
            return false;
        }
    }
    return true;
}

bool asciiStartsWithIgnoreCase(std::string_view text, std::string_view prefix) {
    if (text.size() < prefix.size()) {
        return false;
    }
    return asciiEqualsIgnoreCase(text.substr(0, prefix.size()), prefix);
}

}  // namespace

bool isValidUtf8(std::string_view s) {
    size_t pos = 0;
    while (pos < s.size()) {
        size_t step = 0;
        uint32_t cp = 0;
        if (!decodeOneUtf8(s, pos, step, cp)) {
            return false;
        }
        pos += step;
    }
    return true;
}

SurroundingSlice extractSurroundingContext(std::string_view utf8_text,
                                           size_t cursor_chars,
                                           size_t anchor_chars,
                                           size_t max_before_chars,
                                           size_t max_after_chars) {
    struct Span {
        size_t offset;
        size_t len;
        bool valid;
    };
    std::vector<Span> spans;
    spans.reserve(std::min<size_t>(utf8_text.size(), 512));

    size_t pos = 0;
    bool all_valid = true;
    while (pos < utf8_text.size()) {
        size_t step = 0;
        uint32_t cp = 0;
        const bool ok = decodeOneUtf8(utf8_text, pos, step, cp);
        if (!ok) {
            all_valid = false;
            step = 1;
        }
        spans.push_back(Span{pos, step, ok});
        pos += step;
    }

    const size_t total = spans.size();
    const size_t start_char = std::min({cursor_chars, anchor_chars, total});
    const size_t end_char = std::min(std::max(cursor_chars, anchor_chars), total);

    const size_t before_begin = (start_char > max_before_chars) ? (start_char - max_before_chars) : 0;
    const size_t after_end = std::min(total, end_char + max_after_chars);

    SurroundingSlice result;
    if (all_valid) {
        if (before_begin < start_char) {
            const size_t byte_start = spans[before_begin].offset;
            const size_t byte_end =
                (start_char < total) ? spans[start_char].offset : utf8_text.size();
            result.before.assign(utf8_text.substr(byte_start, byte_end - byte_start));
        }
        if (end_char < after_end) {
            const size_t byte_start = spans[end_char].offset;
            const size_t byte_end =
                (after_end < total) ? spans[after_end].offset : utf8_text.size();
            result.after.assign(utf8_text.substr(byte_start, byte_end - byte_start));
        }
        return result;
    }

    // 含损坏字节的退化路径：用 UTF-8 替代符 U+FFFD 替代非法字节，确保 IPC 上报必为合法 UTF-8。
    auto collect_range = [&](size_t from, size_t to, std::string &dst) {
        for (size_t i = from; i < to; ++i) {
            if (spans[i].valid) {
                dst.append(utf8_text.substr(spans[i].offset, spans[i].len));
            } else {
                dst.append("\xEF\xBF\xBD");
            }
        }
    };
    collect_range(before_begin, start_char, result.before);
    collect_range(end_char, after_end, result.after);
    return result;
}

size_t utf8CharOffsetToByteOffset(std::string_view utf8_text, size_t char_offset) {
    size_t pos = 0;
    size_t count = 0;
    while (pos < utf8_text.size() && count < char_offset) {
        size_t step = 0;
        uint32_t cp = 0;
        decodeOneUtf8(utf8_text, pos, step, cp);
        pos += (step > 0 ? step : 1);
        ++count;
    }
    return pos;
}

bool isAsciiLetter(uint32_t cp) {
    return (cp >= 'a' && cp <= 'z') || (cp >= 'A' && cp <= 'Z');
}

bool isComposingWordChar(uint32_t cp) {
    return isAsciiLetter(cp) || cp == '\'' || cp == '-';
}

bool isClosingPunctuation(uint32_t cp) {
    switch (cp) {
        case '.':
        case ',':
        case '?':
        case '!':
        case ';':
        case ':':
        case ')':
        case ']':
        case '"':
            return true;
        default:
            return false;
    }
}

InputSpellingStatus classifyInputSpelling(
    std::string_view preedit,
    const std::vector<ipc::Candidate> &candidates) {
    const std::string_view trimmed = trimAsciiSpaces(preedit);
    if (trimmed.empty()) {
        return InputSpellingStatus::PrefixInProgress;
    }

    if (trimmed.find(' ') == std::string_view::npos) {
        bool has_exact = false;
        bool has_completion = false;
        for (const auto &c : candidates) {
            if (c.kind == ipc::CandidateKind::Exact) {
                has_exact = true;
                break;
            }
            if (c.kind == ipc::CandidateKind::Completion &&
                asciiStartsWithIgnoreCase(c.text, trimmed)) {
                has_completion = true;
            }
        }
        if (has_exact) {
            return InputSpellingStatus::ExactWord;
        }
        if (has_completion) {
            return InputSpellingStatus::PrefixInProgress;
        }
        return InputSpellingStatus::LikelyMisspelled;
    }

    bool has_raw = false;
    for (const auto &c : candidates) {
        if (c.kind == ipc::CandidateKind::Sentence &&
            asciiEqualsIgnoreCase(trimmed, c.text)) {
            return InputSpellingStatus::ExactWord;
        }
        if (c.kind == ipc::CandidateKind::Raw) {
            has_raw = true;
        }
    }
    return has_raw ? InputSpellingStatus::LikelyMisspelled
                   : InputSpellingStatus::PrefixInProgress;
}

std::string_view formatCandidateComment(const ipc::Candidate &cand,
                                        size_t index,
                                        std::string_view preedit,
                                        InputSpellingStatus status,
                                        bool show_comments) {
    if (!show_comments) {
        return {};
    }
    switch (cand.kind) {
        case ipc::CandidateKind::Exact:
            if (cand.comment && *cand.comment == "partial") {
                return "\xE2\x9C\x93 partial";
            }
            return "\xE2\x9C\x93";
        case ipc::CandidateKind::Completion:
            if (cand.comment && *cand.comment == "partial") {
                return "partial";
            }
            return "\xE2\x80\xA6";
        case ipc::CandidateKind::TypoCorrection:
            return "typo";
        case ipc::CandidateKind::PhoneticCorrection:
            return "phonetic";
        case ipc::CandidateKind::Continuation:
            if (cand.comment && !cand.comment->empty() && *cand.comment != "\xE2\x9C\xA6") {
                return *cand.comment;
            }
            return "next";
        case ipc::CandidateKind::Sentence:
            if (asciiEqualsIgnoreCase(trimAsciiSpaces(preedit), cand.text)) {
                return "\xE2\x9C\x93";
            }
            if (index == 0 && status == InputSpellingStatus::LikelyMisspelled) {
                return "fix";
            }
            return {};
        case ipc::CandidateKind::Raw:
            return (status == InputSpellingStatus::LikelyMisspelled) ? "? raw" : "raw";
    }
    return {};
}

CandidatePresentationPlan decideCandidatePresentation(
    std::string_view preedit,
    const std::vector<ipc::Candidate> &candidates,
    const CandidatePresentationOptions &opts) {
    CandidatePresentationPlan plan;
    const size_t max_cap = std::clamp(opts.max_candidates, size_t{1}, size_t{9});
    const size_t inspect_count = std::min(candidates.size(), max_cap);
    const auto status = classifyInputSpelling(preedit, candidates);

    bool has_multiword = false;
    size_t max_single_chars = 0;
    size_t total_display_chars = 0;

    for (size_t i = 0; i < inspect_count; ++i) {
        const auto &cand = candidates[i];
        if (cand.kind == ipc::CandidateKind::Sentence ||
            cand.text.find(' ') != std::string::npos) {
            has_multiword = true;
        }
        const size_t text_chars = countUtf8Chars(cand.text);
        max_single_chars = std::max(max_single_chars, text_chars);

        // 估算本项在横排条中的显示列宽：序号 "N. " (3) + 文本字符数 + 可选注释
        total_display_chars += text_chars + 3;
        const auto comment =
            formatCandidateComment(cand, i, preedit, status, opts.show_comments);
        if (!comment.empty()) {
            total_display_chars += 1 + countUtf8Chars(comment);
        }
    }

    plan.is_long_mode =
        (has_multiword && (max_single_chars >= 12 || total_display_chars >= 44)) ||
        total_display_chars >= 64;

    switch (opts.layout) {
        case LayoutPreference::Horizontal:
            plan.layout = ResolvedLayout::Horizontal;
            break;
        case LayoutPreference::Vertical:
            plan.layout = ResolvedLayout::Vertical;
            break;
        case LayoutPreference::Auto:
            plan.layout =
                plan.is_long_mode ? ResolvedLayout::Vertical : ResolvedLayout::Default;
            break;
    }

    if (plan.is_long_mode) {
        const size_t min_long = std::min<size_t>(3, max_cap);
        plan.page_size = std::clamp(opts.long_page_size, min_long, max_cap);
    } else {
        plan.page_size = max_cap;
    }
    return plan;
}

void ShiftTapDetector::onKeyPress(uint32_t sym,
                                  bool is_shift_sym,
                                  bool has_non_shift_mods,
                                  bool is_repeat) {
    if (is_shift_sym && !has_non_shift_mods && !is_repeat) {
        pending_ = true;
        pending_sym_ = sym;
    } else {
        // 只要途中按下任意其他键、叠加修饰键或触发长按连发，立刻作废本次单击意图
        pending_ = false;
        pending_sym_ = 0;
    }
}

bool ShiftTapDetector::onKeyRelease(uint32_t sym, bool is_shift_sym, bool has_non_shift_mods) {
    const bool matched = pending_ && is_shift_sym && sym == pending_sym_ && !has_non_shift_mods;
    pending_ = false;
    pending_sym_ = 0;
    return matched;
}

void ShiftTapDetector::reset() {
    pending_ = false;
    pending_sym_ = 0;
}

uint64_t SessionState::nextSeq() {
    return ++last_sent_seq_;
}

bool SessionState::acceptState(const ipc::PreeditState &state) {
    if (state.seq < last_sent_seq_) {
        return false;
    }
    if (state.seq <= reset_seq_ && !state.preedit.empty()) {
        return false;
    }
    if (state.seq == last_applied_seq_ && state.revision <= last_applied_revision_) {
        return false;
    }
    last_applied_seq_ = state.seq;
    last_applied_revision_ = state.revision;
    confirmed_preedit_ = state.preedit;
    confirmed_cursor_ = state.cursor;
    predicted_preedit_ = state.preedit;
    candidates_ = state.candidates;
    return true;
}

CommitApplyResult SessionState::acceptCommit(const ipc::CommitAction &commit) {
    CommitApplyResult result;
    if (commit.seq <= reset_seq_ || commit.seq <= last_committed_seq_) {
        return result;
    }

    result.apply_commit = true;
    last_committed_seq_ = commit.seq;

    // 仅当客户端尚未发出更新的按键请求时，才用本条 commit 刷新当前预编辑与候选面板。
    if (commit.seq >= last_sent_seq_) {
        result.update_panel = true;
        last_applied_seq_ = commit.seq;
        last_applied_revision_ = 0;
        confirmed_preedit_ = commit.remaining_preedit;
        confirmed_cursor_ = countUtf8Chars(commit.remaining_preedit);
        predicted_preedit_ = commit.remaining_preedit;
        candidates_ = commit.candidates;

        // 只有在“刚完成整词上屏且服务端附带智能尾空格、且非 Enter 原串上屏”时激活标点回删记忆
        last_auto_space_ = !last_commit_was_enter_ && commit.delete_before == 0 &&
                           commit.remaining_preedit.empty() && !commit.text.empty() &&
                           commit.text.back() == ' ';
        last_commit_was_enter_ = false;
    }
    return result;
}

void SessionState::onLocalChar(char ch) {
    predicted_preedit_.push_back(ch);
    last_auto_space_ = false;
    last_commit_was_enter_ = false;
}

void SessionState::onLocalClosingPunct() {
    last_auto_space_ = false;
    last_commit_was_enter_ = false;
}

void SessionState::onLocalSpace(bool space_commits) {
    last_auto_space_ = false;
    last_commit_was_enter_ = false;
    if (predicted_preedit_.empty()) {
        return;
    }
    if (space_commits) {
        onLocalSelect(0);
    } else if (predicted_preedit_.back() != ' ') {
        predicted_preedit_.push_back(' ');
    }
}

void SessionState::onLocalBackspace() {
    last_auto_space_ = false;
    last_commit_was_enter_ = false;
    if (!predicted_preedit_.empty()) {
        popLastUtf8Codepoint(predicted_preedit_);
    }
}

void SessionState::onLocalEnter() {
    predicted_preedit_.clear();
    last_auto_space_ = false;
    last_commit_was_enter_ = true;
}

void SessionState::onLocalEscape() {
    predicted_preedit_.clear();
    last_auto_space_ = false;
    last_commit_was_enter_ = false;
}

void SessionState::onLocalSelect(size_t index) {
    last_auto_space_ = false;
    last_commit_was_enter_ = false;
    if (index < candidates_.size()) {
        const size_t total = countUtf8Chars(predicted_preedit_);
        const size_t consumed = candidates_[index].consumed_chars;
        if (consumed > 0 && consumed < total) {
            predicted_preedit_ = skipFirstNCharsAndTrimLeadingSpaces(predicted_preedit_, consumed);
            return;
        }
    }
    predicted_preedit_.clear();
}

void SessionState::onLocalReset(uint64_t reset_seq) {
    reset_seq_ = reset_seq;
    confirmed_preedit_.clear();
    confirmed_cursor_ = 0;
    candidates_.clear();
    predicted_preedit_.clear();
    last_auto_space_ = false;
    last_commit_was_enter_ = false;
    last_applied_revision_ = 0;
}

void SessionState::onDisconnected() {
    confirmed_preedit_.clear();
    confirmed_cursor_ = 0;
    candidates_.clear();
    predicted_preedit_.clear();
    last_auto_space_ = false;
    last_commit_was_enter_ = false;
    has_sent_context_ = false;
    last_sent_context_before_.clear();
    last_sent_context_after_.clear();
    has_sent_privacy_ = false;
}

bool SessionState::shouldSendContext(std::string_view before, std::string_view after) const {
    return !has_sent_context_ || last_sent_context_before_ != before ||
           last_sent_context_after_ != after;
}

void SessionState::markContextSent(std::string before, std::string after) {
    has_sent_context_ = true;
    last_sent_context_before_ = std::move(before);
    last_sent_context_after_ = std::move(after);
}

bool SessionState::shouldSendPrivacy(bool enabled) const {
    return !has_sent_privacy_ || last_sent_privacy_ != enabled;
}

void SessionState::markPrivacySent(bool enabled) {
    has_sent_privacy_ = true;
    last_sent_privacy_ = enabled;
}

}  // namespace anime
