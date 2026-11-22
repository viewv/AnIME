#ifndef ANIME_FCITX5_SESSION_STATE_H
#define ANIME_FCITX5_SESSION_STATE_H

#include "ipc_protocol.h"

#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

namespace anime {

struct SurroundingSlice {
    std::string before;
    std::string after;
};

// 从宿主应用 surrounding text 中按 Unicode 码点安全截断，绝不在多字节 UTF-8 序列中间切断。
SurroundingSlice extractSurroundingContext(std::string_view utf8_text,
                                           size_t cursor_chars,
                                           size_t anchor_chars,
                                           size_t max_before_chars = 256,
                                           size_t max_after_chars = 64);

size_t utf8CharOffsetToByteOffset(std::string_view utf8_text, size_t char_offset);
bool isValidUtf8(std::string_view s);

// 仅用于前端同步判定“是否拦截按键”，不含词典/纠错/语言模型逻辑。
bool isAsciiLetter(uint32_t cp);
bool isComposingWordChar(uint32_t cp);
bool isClosingPunctuation(uint32_t cp);

enum class LayoutPreference {
    Auto,
    Horizontal,
    Vertical,
};

enum class ResolvedLayout {
    Default,
    Horizontal,
    Vertical,
};

struct CandidatePresentationOptions {
    LayoutPreference layout = LayoutPreference::Auto;
    size_t max_candidates = 9;
    size_t long_page_size = 5;
    bool show_comments = true;
};

struct CandidatePresentationPlan {
    ResolvedLayout layout = ResolvedLayout::Default;
    size_t page_size = 9;
    bool is_long_mode = false;
};

enum class InputSpellingStatus {
    ExactWord,
    PrefixInProgress,
    LikelyMisspelled,
};

// 基于 animed 返回的 CandidateKind 与当前 preedit，判断用户原输入是否命中真实英语词典（纯展示分类，不含词典逻辑）。
InputSpellingStatus classifyInputSpelling(
    std::string_view preedit,
    const std::vector<ipc::Candidate> &candidates);

// 统一候选注释展示规则：用 "✓" 标识词典真实存在的词/合法整句，
// 用 "typo"/"phonetic"/"fix"/"? raw" 醒目提示用户可能输入了错词或未收录词，
// 并省略整句次要候选的重复标签以减少视觉噪音和横向占位。
std::string_view formatCandidateComment(const ipc::Candidate &cand,
                                        size_t index,
                                        std::string_view preedit,
                                        InputSpellingStatus status,
                                        bool show_comments);

// 长句/宽候选自适应排版决策（纯逻辑，零 Fcitx5 依赖）：
// Auto 模式下，一旦出现多词整句/短语候选或候选条总展示宽度过大，自动切为竖排并限制每页显示条数，
// 防止横排候选框超出屏幕。
CandidatePresentationPlan decideCandidatePresentation(
    std::string_view preedit,
    const std::vector<ipc::Candidate> &candidates,
    const CandidatePresentationOptions &opts);

// 单按 Shift 切换直通状态机：
// 只有在无其他修饰键、非长按自动连发、且中间未按任何其他键的情况下按下并释放同一 Shift 键才触发切换。
class ShiftTapDetector {
public:
    void onKeyPress(uint32_t sym, bool is_shift_sym, bool has_non_shift_mods, bool is_repeat);
    bool onKeyRelease(uint32_t sym, bool is_shift_sym, bool has_non_shift_mods);
    void reset();

private:
    bool pending_ = false;
    uint32_t pending_sym_ = 0;
};

struct CommitApplyResult {
    bool apply_commit = false;
    bool update_panel = false;
};

// 会话状态与 seq/revision 防过期控制器：
// 1. 严格执行 ipc-protocol.md §2：丢弃 msg.seq < last_sent_seq 或同 seq 下 revision <= last_applied_revision 的 state；
// 2. 区分 commit 与 state 的时序：若用户选词后极快敲入下一词首字母，晚到的旧 seq commit 仍需上屏以免丢字，但不再覆盖新输入面板；
// 3. 维护轻量乐观 preedit 预测，使 Fcitx5 同步 keyEvent 能在异步回包到达前立即判断是否继续拦截 Space/Backspace/1-9/Enter。
class SessionState {
public:
    uint64_t nextSeq();

    uint64_t lastSentSeq() const { return last_sent_seq_; }
    uint64_t lastAppliedSeq() const { return last_applied_seq_; }
    uint32_t lastAppliedRevision() const { return last_applied_revision_; }
    uint64_t lastCommittedSeq() const { return last_committed_seq_; }
    uint64_t resetSeq() const { return reset_seq_; }

    bool isComposing() const { return !predicted_preedit_.empty(); }
    const std::string &effectivePreedit() const { return predicted_preedit_; }
    const std::string &confirmedPreedit() const { return confirmed_preedit_; }
    size_t confirmedCursor() const { return confirmed_cursor_; }
    const std::vector<ipc::Candidate> &candidates() const { return candidates_; }
    bool lastAutoSpace() const { return last_auto_space_; }

    bool acceptState(const ipc::PreeditState &state);
    CommitApplyResult acceptCommit(const ipc::CommitAction &commit);

    void onLocalChar(char ch);
    void onLocalClosingPunct();
    void onLocalSpace(bool space_commits);
    void onLocalBackspace();
    void onLocalEnter();
    void onLocalEscape();
    void onLocalSelect(size_t index);
    void onLocalReset(uint64_t reset_seq);
    void onDisconnected();

    bool shouldSendContext(std::string_view before, std::string_view after) const;
    void markContextSent(std::string before, std::string after);

    bool shouldSendPrivacy(bool enabled) const;
    void markPrivacySent(bool enabled);

    void clearLastAutoSpace() { last_auto_space_ = false; }

private:
    uint64_t last_sent_seq_ = 0;
    uint64_t last_applied_seq_ = 0;
    uint32_t last_applied_revision_ = 0;
    uint64_t last_committed_seq_ = 0;
    uint64_t reset_seq_ = 0;

    std::string confirmed_preedit_;
    size_t confirmed_cursor_ = 0;
    std::vector<ipc::Candidate> candidates_;

    std::string predicted_preedit_;
    bool last_auto_space_ = false;
    bool last_commit_was_enter_ = false;

    bool has_sent_context_ = false;
    std::string last_sent_context_before_;
    std::string last_sent_context_after_;

    bool has_sent_privacy_ = false;
    bool last_sent_privacy_ = false;
};

}  // namespace anime

#endif  // ANIME_FCITX5_SESSION_STATE_H
