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
