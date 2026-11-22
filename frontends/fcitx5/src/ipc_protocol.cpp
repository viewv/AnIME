#include "ipc_protocol.h"

#include <charconv>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <limits>
#include <utility>

namespace anime::ipc {
namespace {

constexpr size_t kMaxJsonDepth = 32;

void appendJsonString(std::string &out, std::string_view s) {
    out.push_back('"');
    for (unsigned char c : s) {
        switch (c) {
            case '"':
                out.append("\\\"");
                break;
            case '\\':
                out.append("\\\\");
                break;
            case '\b':
                out.append("\\b");
                break;
            case '\f':
                out.append("\\f");
                break;
            case '\n':
                out.append("\\n");
                break;
            case '\r':
                out.append("\\r");
                break;
            case '\t':
                out.append("\\t");
                break;
            default:
                if (c < 0x20U) {
                    char buf[7];
                    std::snprintf(buf, sizeof(buf), "\\u%04x", static_cast<unsigned int>(c));
                    out.append(buf);
                } else {
                    out.push_back(static_cast<char>(c));
                }
                break;
        }
    }
    out.push_back('"');
}

std::optional<std::string> finalizeLine(std::string out) {
    if (out.size() > kMaxIpcLineBytes) {
        return std::nullopt;
    }
    if (out.find('\n') != std::string::npos || out.find('\r') != std::string::npos) {
        return std::nullopt;
    }
    return out;
}

// 验证字符串恰好包含 1 个合法 UTF-8 码点，避免发出违反协议的 key="char" 负载。
bool isSingleUtf8Codepoint(std::string_view s) {
    if (s.empty() || s.size() > 4) {
        return false;
    }
    const auto b0 = static_cast<unsigned char>(s[0]);
    size_t expected_len = 0;
    uint32_t cp = 0;
    if ((b0 & 0x80U) == 0) {
        expected_len = 1;
        cp = b0;
    } else if ((b0 & 0xE0U) == 0xC0U) {
        expected_len = 2;
        cp = b0 & 0x1FU;
    } else if ((b0 & 0xF0U) == 0xE0U) {
        expected_len = 3;
        cp = b0 & 0x0FU;
    } else if ((b0 & 0xF8U) == 0xF0U) {
        expected_len = 4;
        cp = b0 & 0x07U;
    } else {
        return false;
    }
    if (s.size() != expected_len) {
        return false;
    }
    for (size_t i = 1; i < expected_len; ++i) {
        const auto bi = static_cast<unsigned char>(s[i]);
        if ((bi & 0xC0U) != 0x80U) {
            return false;
        }
        cp = (cp << 6U) | (bi & 0x3FU);
    }
    if (expected_len == 2 && cp < 0x80U) return false;
    if (expected_len == 3 && cp < 0x800U) return false;
    if (expected_len == 4 && (cp < 0x10000U || cp > 0x10FFFFU)) return false;
    if (cp >= 0xD800U && cp <= 0xDFFFU) return false;
    return true;
}

struct JsonValue;
using JsonArray = std::vector<JsonValue>;
using JsonObject = std::vector<std::pair<std::string, JsonValue>>;

struct JsonNumber {
    double as_double = 0.0;
    std::optional<uint64_t> as_u64;
};

struct JsonValue {
    std::variant<std::nullptr_t, bool, JsonNumber, std::string, JsonArray, JsonObject> data{
        nullptr};

    bool isNull() const { return std::holds_alternative<std::nullptr_t>(data); }
    const bool *asBool() const { return std::get_if<bool>(&data); }
    const JsonNumber *asNumber() const { return std::get_if<JsonNumber>(&data); }
    const std::string *asString() const { return std::get_if<std::string>(&data); }
    const JsonArray *asArray() const { return std::get_if<JsonArray>(&data); }
    const JsonObject *asObject() const { return std::get_if<JsonObject>(&data); }

    const JsonValue *get(std::string_view key) const {
        const auto *obj = asObject();
        if (!obj) return nullptr;
        // 反向扫描以遵守“重复键后值覆盖前值”语义
        for (auto it = obj->rbegin(); it != obj->rend(); ++it) {
            if (it->first == key) {
                return &it->second;
            }
        }
        return nullptr;
    }
};

void appendUtf8Codepoint(uint32_t cp, std::string &out) {
    if (cp <= 0x7FU) {
        out.push_back(static_cast<char>(cp));
    } else if (cp <= 0x7FFU) {
        out.push_back(static_cast<char>(0xC0U | ((cp >> 6U) & 0x1FU)));
        out.push_back(static_cast<char>(0x80U | (cp & 0x3FU)));
    } else if (cp <= 0xFFFFU) {
        out.push_back(static_cast<char>(0xE0U | ((cp >> 12U) & 0x0FU)));
        out.push_back(static_cast<char>(0x80U | ((cp >> 6U) & 0x3FU)));
        out.push_back(static_cast<char>(0x80U | (cp & 0x3FU)));
    } else {
        out.push_back(static_cast<char>(0xF0U | ((cp >> 18U) & 0x07U)));
        out.push_back(static_cast<char>(0x80U | ((cp >> 12U) & 0x3FU)));
        out.push_back(static_cast<char>(0x80U | ((cp >> 6U) & 0x3FU)));
        out.push_back(static_cast<char>(0x80U | (cp & 0x3FU)));
    }
}

class JsonParser {
public:
    explicit JsonParser(std::string_view input) : input_(input) {}

    std::optional<JsonValue> parseRoot() {
        skipWs();
        auto val = parseValue(0);
        if (!val) return std::nullopt;
        skipWs();
        if (pos_ != input_.size()) return std::nullopt;
        return val;
    }

private:
    std::string_view input_;
    size_t pos_ = 0;

    void skipWs() {
        while (pos_ < input_.size()) {
            char c = input_[pos_];
            if (c == ' ' || c == '\t' || c == '\n' || c == '\r') {
                ++pos_;
            } else {
                break;
            }
        }
    }

    bool consumeLiteral(std::string_view lit) {
        if (input_.substr(pos_, lit.size()) == lit) {
            pos_ += lit.size();
            return true;
        }
        return false;
    }

    std::optional<uint16_t> parseHex4() {
        if (pos_ + 4 > input_.size()) return std::nullopt;
        uint16_t val = 0;
        for (size_t i = 0; i < 4; ++i) {
            char c = input_[pos_++];
            val <<= 4U;
            if (c >= '0' && c <= '9') {
                val |= static_cast<uint16_t>(c - '0');
            } else if (c >= 'a' && c <= 'f') {
                val |= static_cast<uint16_t>(c - 'a' + 10);
            } else if (c >= 'A' && c <= 'F') {
                val |= static_cast<uint16_t>(c - 'A' + 10);
            } else {
                return std::nullopt;
            }
        }
        return val;
    }

    std::optional<std::string> parseString() {
        if (pos_ >= input_.size() || input_[pos_] != '"') return std::nullopt;
        ++pos_;
        std::string out;
        while (pos_ < input_.size()) {
            const auto ch = static_cast<unsigned char>(input_[pos_++]);
            if (ch == '"') {
                return out;
            }
            if (ch < 0x20U) {
                return std::nullopt;
            }
            if (ch != '\\') {
                out.push_back(static_cast<char>(ch));
                continue;
            }
            if (pos_ >= input_.size()) return std::nullopt;
            const char esc = input_[pos_++];
            switch (esc) {
                case '"':
                    out.push_back('"');
                    break;
                case '\\':
                    out.push_back('\\');
                    break;
                case '/':
                    out.push_back('/');
                    break;
                case 'b':
                    out.push_back('\b');
                    break;
                case 'f':
                    out.push_back('\f');
                    break;
                case 'n':
                    out.push_back('\n');
                    break;
                case 'r':
                    out.push_back('\r');
                    break;
                case 't':
                    out.push_back('\t');
                    break;
                case 'u': {
                    auto u1 = parseHex4();
                    if (!u1) return std::nullopt;
                    uint32_t cp = *u1;
                    if (cp >= 0xD800U && cp <= 0xDBFFU) {
                        if (!consumeLiteral("\\u")) return std::nullopt;
                        auto u2 = parseHex4();
                        if (!u2 || *u2 < 0xDC00U || *u2 > 0xDFFFU) return std::nullopt;
                        cp = 0x10000U + (((cp - 0xD800U) << 10U) | (static_cast<uint32_t>(*u2) - 0xDC00U));
                    } else if (cp >= 0xDC00U && cp <= 0xDFFFU) {
                        return std::nullopt;
                    }
                    appendUtf8Codepoint(cp, out);
                    break;
                }
                default:
                    return std::nullopt;
            }
        }
        return std::nullopt;
    }

    std::optional<JsonNumber> parseNumber() {
        const size_t start = pos_;
        bool is_nonneg_int = true;
        if (pos_ < input_.size() && input_[pos_] == '-') {
            is_nonneg_int = false;
            ++pos_;
        }
        if (pos_ >= input_.size()) return std::nullopt;
        if (input_[pos_] == '0') {
            ++pos_;
            if (pos_ < input_.size() && input_[pos_] >= '0' && input_[pos_] <= '9') {
                return std::nullopt;
            }
        } else if (input_[pos_] >= '1' && input_[pos_] <= '9') {
            while (pos_ < input_.size() && input_[pos_] >= '0' && input_[pos_] <= '9') {
                ++pos_;
            }
        } else {
            return std::nullopt;
        }
        if (pos_ < input_.size() && input_[pos_] == '.') {
            is_nonneg_int = false;
            ++pos_;
            if (pos_ >= input_.size() || input_[pos_] < '0' || input_[pos_] > '9') {
                return std::nullopt;
            }
            while (pos_ < input_.size() && input_[pos_] >= '0' && input_[pos_] <= '9') {
                ++pos_;
            }
        }
        if (pos_ < input_.size() && (input_[pos_] == 'e' || input_[pos_] == 'E')) {
            is_nonneg_int = false;
            ++pos_;
            if (pos_ < input_.size() && (input_[pos_] == '+' || input_[pos_] == '-')) {
                ++pos_;
            }
            if (pos_ >= input_.size() || input_[pos_] < '0' || input_[pos_] > '9') {
                return std::nullopt;
            }
            while (pos_ < input_.size() && input_[pos_] >= '0' && input_[pos_] <= '9') {
                ++pos_;
            }
        }

        const std::string_view token = input_.substr(start, pos_ - start);
        JsonNumber num;
        if (is_nonneg_int) {
            uint64_t uval = 0;
            const auto [ptr, ec] = std::from_chars(token.data(), token.data() + token.size(), uval);
            if (ec == std::errc{} && ptr == token.data() + token.size()) {
                num.as_u64 = uval;
            }
        }
        std::string tmp(token);
        char *endptr = nullptr;
        const double dval = std::strtod(tmp.c_str(), &endptr);
        if (endptr != tmp.c_str() + tmp.size() || !std::isfinite(dval)) {
            return std::nullopt;
        }
        num.as_double = dval;
        return num;
    }

    std::optional<JsonValue> parseValue(size_t depth) {
        if (depth > kMaxJsonDepth || pos_ >= input_.size()) return std::nullopt;
        const char c = input_[pos_];
        if (c == 'n') {
            if (!consumeLiteral("null")) return std::nullopt;
            return JsonValue{nullptr};
        }
        if (c == 't') {
            if (!consumeLiteral("true")) return std::nullopt;
            return JsonValue{true};
        }
        if (c == 'f') {
            if (!consumeLiteral("false")) return std::nullopt;
            return JsonValue{false};
        }
        if (c == '"') {
            auto s = parseString();
            if (!s) return std::nullopt;
            return JsonValue{std::move(*s)};
        }
        if (c == '-' || (c >= '0' && c <= '9')) {
            auto n = parseNumber();
            if (!n) return std::nullopt;
            return JsonValue{*n};
        }
        if (c == '[') {
            ++pos_;
            JsonArray arr;
            skipWs();
            if (pos_ < input_.size() && input_[pos_] == ']') {
                ++pos_;
                return JsonValue{std::move(arr)};
            }
            while (true) {
                skipWs();
                auto item = parseValue(depth + 1);
                if (!item) return std::nullopt;
                arr.push_back(std::move(*item));
                skipWs();
                if (pos_ >= input_.size()) return std::nullopt;
                if (input_[pos_] == ']') {
                    ++pos_;
                    return JsonValue{std::move(arr)};
                }
                if (input_[pos_] != ',') return std::nullopt;
                ++pos_;
            }
        }
        if (c == '{') {
            ++pos_;
            JsonObject obj;
            skipWs();
            if (pos_ < input_.size() && input_[pos_] == '}') {
                ++pos_;
                return JsonValue{std::move(obj)};
            }
            while (true) {
                skipWs();
                auto key = parseString();
                if (!key) return std::nullopt;
                skipWs();
                if (pos_ >= input_.size() || input_[pos_] != ':') return std::nullopt;
                ++pos_;
                skipWs();
                auto val = parseValue(depth + 1);
                if (!val) return std::nullopt;
                obj.emplace_back(std::move(*key), std::move(*val));
                skipWs();
                if (pos_ >= input_.size()) return std::nullopt;
                if (input_[pos_] == '}') {
                    ++pos_;
                    return JsonValue{std::move(obj)};
                }
                if (input_[pos_] != ',') return std::nullopt;
                ++pos_;
            }
        }
        return std::nullopt;
    }
};

std::optional<std::string_view> reqStr(const JsonValue &obj, std::string_view field) {
    const JsonValue *v = obj.get(field);
    if (!v) return std::nullopt;
    const auto *s = v->asString();
    if (!s) return std::nullopt;
    return std::string_view(*s);
}

std::optional<std::optional<std::string>> optStr(const JsonValue &obj, std::string_view field) {
    const JsonValue *v = obj.get(field);
    if (!v || v->isNull()) return std::optional<std::string>{std::nullopt};
    const auto *s = v->asString();
    if (!s) return std::nullopt;
    return std::optional<std::string>{*s};
}

std::optional<bool> reqBool(const JsonValue &obj, std::string_view field) {
    const JsonValue *v = obj.get(field);
    if (!v) return std::nullopt;
    const auto *b = v->asBool();
    if (!b) return std::nullopt;
    return *b;
}

std::optional<uint64_t> reqU64(const JsonValue &obj, std::string_view field) {
    const JsonValue *v = obj.get(field);
    if (!v) return std::nullopt;
    const auto *n = v->asNumber();
    if (!n || !n->as_u64) return std::nullopt;
    return *n->as_u64;
}

std::optional<std::optional<uint64_t>> optU64(const JsonValue &obj, std::string_view field) {
    const JsonValue *v = obj.get(field);
    if (!v || v->isNull()) return std::optional<uint64_t>{std::nullopt};
    const auto *n = v->asNumber();
    if (!n || !n->as_u64) return std::nullopt;
    return std::optional<uint64_t>{*n->as_u64};
}

std::optional<uint32_t> reqU32(const JsonValue &obj, std::string_view field) {
    auto v = reqU64(obj, field);
    if (!v || *v > std::numeric_limits<uint32_t>::max()) return std::nullopt;
    return static_cast<uint32_t>(*v);
}

std::optional<size_t> reqSizeT(const JsonValue &obj, std::string_view field) {
    auto v = reqU64(obj, field);
    if (!v || *v > std::numeric_limits<size_t>::max()) return std::nullopt;
    return static_cast<size_t>(*v);
}

std::optional<float> reqF32(const JsonValue &obj, std::string_view field) {
    const JsonValue *v = obj.get(field);
    if (!v) return std::nullopt;
    const auto *n = v->asNumber();
    if (!n) return std::nullopt;
    const auto f = static_cast<float>(n->as_double);
    if (!std::isfinite(f)) return std::nullopt;
    return f;
}

std::optional<Candidate> decodeCandidate(const JsonValue &val) {
    if (!val.asObject()) return std::nullopt;
    auto text = reqStr(val, "text");
    auto kind_str = reqStr(val, "kind");
    if (!text || !kind_str) return std::nullopt;
    auto kind = candidateKindFromString(*kind_str);
    auto comment = optStr(val, "comment");
    auto score = reqF32(val, "score");
    auto consumed_chars = reqSizeT(val, "consumed_chars");
    auto trailing_space = reqBool(val, "trailing_space");
    if (!kind || !comment || !score || !consumed_chars || !trailing_space) {
        return std::nullopt;
    }
    return Candidate{
        std::string(*text),
        *kind,
        std::move(*comment),
        *score,
        *consumed_chars,
        *trailing_space,
    };
}

std::optional<std::vector<Candidate>> decodeCandidates(const JsonValue &obj) {
    const JsonValue *v = obj.get("candidates");
    if (!v) return std::nullopt;
    const auto *arr = v->asArray();
    if (!arr) return std::nullopt;
    std::vector<Candidate> out;
    out.reserve(arr->size());
    for (const auto &item : *arr) {
        auto c = decodeCandidate(item);
        if (!c) return std::nullopt;
        out.push_back(std::move(*c));
    }
    return out;
}

}  // namespace

std::string_view candidateKindToString(CandidateKind kind) {
    switch (kind) {
        case CandidateKind::Exact:
            return "Exact";
        case CandidateKind::Completion:
            return "Completion";
        case CandidateKind::TypoCorrection:
            return "TypoCorrection";
        case CandidateKind::PhoneticCorrection:
            return "PhoneticCorrection";
        case CandidateKind::Sentence:
            return "Sentence";
        case CandidateKind::Continuation:
            return "Continuation";
        case CandidateKind::Raw:
            return "Raw";
    }
    return "Raw";
}

std::optional<CandidateKind> candidateKindFromString(std::string_view s) {
    if (s == "Exact") return CandidateKind::Exact;
    if (s == "Completion") return CandidateKind::Completion;
    if (s == "TypoCorrection") return CandidateKind::TypoCorrection;
    if (s == "PhoneticCorrection") return CandidateKind::PhoneticCorrection;
    if (s == "Sentence") return CandidateKind::Sentence;
    if (s == "Continuation") return CandidateKind::Continuation;
    if (s == "Raw") return CandidateKind::Raw;
    return std::nullopt;
}

std::optional<std::string> encodeHello(std::string_view client,
                                       uint32_t protocol_version,
                                       std::optional<std::string_view> session_id) {
    std::string out = R"({"type":"hello","client":)";
    appendJsonString(out, client);
    out.append(R"(,"protocol_version":)");
    out.append(std::to_string(protocol_version));
    if (session_id) {
        out.append(R"(,"session_id":)");
        appendJsonString(out, *session_id);
    }
    out.push_back('}');
    return finalizeLine(std::move(out));
}

std::optional<std::string> encodeKeyChar(uint64_t seq, std::string_view utf8_char, uint32_t mods) {
    if (!isSingleUtf8Codepoint(utf8_char)) {
        return std::nullopt;
    }
    std::string out = R"({"type":"key","seq":)";
    out.append(std::to_string(seq));
    out.append(R"(,"key":"char","text":)");
    appendJsonString(out, utf8_char);
    if (mods != 0) {
        out.append(R"(,"mods":)");
        out.append(std::to_string(mods));
    }
    out.push_back('}');
    return finalizeLine(std::move(out));
}

std::optional<std::string> encodeKeySpecial(uint64_t seq, std::string_view key_name, uint32_t mods) {
    if (key_name != "space" && key_name != "backspace" && key_name != "enter" &&
        key_name != "escape") {
        return std::nullopt;
    }
    std::string out = R"({"type":"key","seq":)";
    out.append(std::to_string(seq));
    out.append(R"(,"key":)");
    appendJsonString(out, key_name);
    if (mods != 0) {
        out.append(R"(,"mods":)");
        out.append(std::to_string(mods));
    }
    out.push_back('}');
    return finalizeLine(std::move(out));
}

std::optional<std::string> encodeKeyDigit(uint64_t seq, uint8_t digit, uint32_t mods) {
    if (digit > 9) {
        return std::nullopt;
    }
    const char digit_ch = static_cast<char>('0' + digit);
    std::string out = R"({"type":"key","seq":)";
    out.append(std::to_string(seq));
    out.append(R"(,"key":"digit","text":")");
    out.push_back(digit_ch);
    out.push_back('"');
    if (mods != 0) {
        out.append(R"(,"mods":)");
        out.append(std::to_string(mods));
    }
    out.push_back('}');
    return finalizeLine(std::move(out));
}

std::optional<std::string> encodeSelect(uint64_t seq, size_t index) {
    std::string out = R"({"type":"select","seq":)";
    out.append(std::to_string(seq));
    out.append(R"(,"index":)");
    out.append(std::to_string(index));
    out.push_back('}');
    return finalizeLine(std::move(out));
}

std::optional<std::string> encodeContext(std::string_view before, std::string_view after) {
    std::string out = R"({"type":"context","before":)";
    appendJsonString(out, before);
    out.append(R"(,"after":)");
    appendJsonString(out, after);
    out.push_back('}');
    return finalizeLine(std::move(out));
}

std::optional<std::string> encodePrivacy(bool enabled) {
    std::string out = R"({"type":"privacy","enabled":)";
    out.append(enabled ? "true}" : "false}");
    return out;
}

std::optional<std::string> encodeReset(uint64_t seq) {
    std::string out = R"({"type":"reset","seq":)";
    out.append(std::to_string(seq));
    out.push_back('}');
    return finalizeLine(std::move(out));
}

std::optional<std::string> encodeConfig(const EngineConfig &cfg) {
    std::string out = R"({"type":"config","space_commits":)";
    out.append(cfg.space_commits ? "true" : "false");
    out.append(R"(,"smart_spacing":)");
    out.append(cfg.smart_spacing ? "true" : "false");
    out.append(R"(,"max_candidates":)");
    out.append(std::to_string(cfg.max_candidates));
    out.append(R"(,"enable_phonetic":)");
    out.append(cfg.enable_phonetic ? "true" : "false");
    out.append(R"(,"enable_neural":)");
    out.append(cfg.enable_neural ? "true" : "false");
    out.push_back('}');
    return finalizeLine(std::move(out));
}

std::optional<std::string> encodePing(std::optional<uint64_t> id) {
    if (!id) {
        return std::string(R"({"type":"ping"})");
    }
    std::string out = R"({"type":"ping","id":)";
    out.append(std::to_string(*id));
    out.push_back('}');
    return finalizeLine(std::move(out));
}

std::optional<IpcResponse> decodeResponseLine(std::string_view line) {
    if (line.size() > kMaxIpcLineBytes) {
        return std::nullopt;
    }
    if (line.ends_with("\r\n")) {
        line.remove_suffix(2);
    } else if (line.ends_with('\n')) {
        line.remove_suffix(1);
    }
    if (line.find('\n') != std::string_view::npos || line.find('\r') != std::string_view::npos) {
        return std::nullopt;
    }

    JsonParser parser(line);
    auto root = parser.parseRoot();
    if (!root || !root->asObject()) {
        return std::nullopt;
    }
    auto msg_type = reqStr(*root, "type");
    if (!msg_type) {
        return std::nullopt;
    }

    if (*msg_type == "hello_ack") {
        auto pv = reqU32(*root, "protocol_version");
        auto version = reqStr(*root, "version");
        const JsonValue *caps_val = root->get("capabilities");
        if (!pv || !version || !caps_val || !caps_val->asArray()) {
            return std::nullopt;
        }
        std::vector<std::string> caps;
        caps.reserve(caps_val->asArray()->size());
        for (const auto &item : *caps_val->asArray()) {
            const auto *s = item.asString();
            if (!s) return std::nullopt;
            caps.push_back(*s);
        }
        return HelloAck{*pv, std::string(*version), std::move(caps)};
    }

    if (*msg_type == "state") {
        auto seq = reqU64(*root, "seq");
        auto revision = reqU32(*root, "revision");
        auto preedit = reqStr(*root, "preedit");
        auto cursor = reqSizeT(*root, "cursor");
        auto candidates = decodeCandidates(*root);
        auto passthrough = reqBool(*root, "passthrough");
        auto final_l3 = reqBool(*root, "final_l3");
        if (!seq || !revision || !preedit || !cursor || !candidates || !passthrough || !final_l3) {
            return std::nullopt;
        }
        return PreeditState{
            *seq,
            *revision,
            std::string(*preedit),
            *cursor,
            std::move(*candidates),
            *passthrough,
            *final_l3,
        };
    }

    if (*msg_type == "commit") {
        auto seq = reqU64(*root, "seq");
        auto text = reqStr(*root, "text");
        auto delete_before = reqSizeT(*root, "delete_before");
        auto remaining_preedit = reqStr(*root, "remaining_preedit");
        auto candidates = decodeCandidates(*root);
        if (!seq || !text || !delete_before || !remaining_preedit || !candidates) {
            return std::nullopt;
        }
        return CommitAction{
            *seq,
            std::string(*text),
            *delete_before,
            std::string(*remaining_preedit),
            std::move(*candidates),
        };
    }

    if (*msg_type == "pong") {
        auto version = reqStr(*root, "version");
        auto id = optU64(*root, "id");
        if (!version || !id) {
            return std::nullopt;
        }
        return Pong{std::string(*version), *id};
    }

    if (*msg_type == "error") {
        auto seq = optU64(*root, "seq");
        auto code = reqStr(*root, "code");
        auto message = reqStr(*root, "message");
        if (!seq || !code || !message) {
            return std::nullopt;
        }
        return ErrorMessage{*seq, std::string(*code), std::string(*message)};
    }

    return std::nullopt;
}

}  // namespace anime::ipc
