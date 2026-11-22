#ifndef ANIME_FCITX5_IPC_PROTOCOL_H
#define ANIME_FCITX5_IPC_PROTOCOL_H

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace anime::ipc {

inline constexpr uint32_t kProtocolVersion = 1;
inline constexpr size_t kMaxIpcLineBytes = 64 * 1024;

enum class CandidateKind {
    Exact,
    Completion,
    TypoCorrection,
    PhoneticCorrection,
    Sentence,
    Continuation,
    Raw,
};

struct Candidate {
    std::string text;
    CandidateKind kind = CandidateKind::Raw;
    std::optional<std::string> comment;
    float score = 0.0F;
    size_t consumed_chars = 0;
    bool trailing_space = false;
};

struct EngineConfig {
    bool space_commits = false;
    bool smart_spacing = true;
    size_t max_candidates = 9;
    bool enable_phonetic = true;
    bool enable_neural = true;
};

struct PreeditState {
    uint64_t seq = 0;
    uint32_t revision = 0;
    std::string preedit;
    size_t cursor = 0;
    std::vector<Candidate> candidates;
    bool passthrough = false;
    bool final_l3 = true;
};

struct CommitAction {
    uint64_t seq = 0;
    std::string text;
    size_t delete_before = 0;
    std::string remaining_preedit;
    std::vector<Candidate> candidates;
};

struct HelloAck {
    uint32_t protocol_version = kProtocolVersion;
    std::string version;
    std::vector<std::string> capabilities;
};

struct Pong {
    std::string version;
    std::optional<uint64_t> id;
};

struct ErrorMessage {
    std::optional<uint64_t> seq;
    std::string code;
    std::string message;
};

using IpcResponse = std::variant<HelloAck, PreeditState, CommitAction, Pong, ErrorMessage>;

// 返回不带尾随 '\n' 的单行 NDJSON 字符串；超过 64 KiB 或入参非法返回 nullopt。
std::optional<std::string> encodeHello(std::string_view client,
                                       uint32_t protocol_version = kProtocolVersion,
                                       std::optional<std::string_view> session_id = std::nullopt);
std::optional<std::string> encodeKeyChar(uint64_t seq, std::string_view utf8_char, uint32_t mods = 0);
std::optional<std::string> encodeKeySpecial(uint64_t seq, std::string_view key_name, uint32_t mods = 0);
std::optional<std::string> encodeKeyDigit(uint64_t seq, uint8_t digit, uint32_t mods = 0);
std::optional<std::string> encodeSelect(uint64_t seq, size_t index);
std::optional<std::string> encodeContext(std::string_view before, std::string_view after);
std::optional<std::string> encodePrivacy(bool enabled);
std::optional<std::string> encodeReset(uint64_t seq);
std::optional<std::string> encodeConfig(const EngineConfig &cfg);
std::optional<std::string> encodePing(std::optional<uint64_t> id = std::nullopt);

// 解析 animed 返回的单行 NDJSON（允许带或不带结尾 \r\n / \n）。
std::optional<IpcResponse> decodeResponseLine(std::string_view line);

std::string_view candidateKindToString(CandidateKind kind);
std::optional<CandidateKind> candidateKindFromString(std::string_view s);

}  // namespace anime::ipc

#endif  // ANIME_FCITX5_IPC_PROTOCOL_H
