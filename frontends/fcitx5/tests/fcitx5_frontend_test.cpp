#include "ipc_client.h"
#include "ipc_protocol.h"
#include "session_state.h"

#include <poll.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

#include <atomic>
#include <cassert>
#include <cerrno>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <string>
#include <string_view>
#include <thread>
#include <variant>
#include <vector>

namespace {

using namespace anime;
using namespace anime::ipc;

void expect(bool cond, const char *msg) {
    if (!cond) {
        std::cerr << "TEST FAILED: " << msg << "\n";
        std::abort();
    }
}

void testProtocolCodec() {
    expect(encodeHello("fcitx5-anime", 1, "wl-1") ==
               R"({"type":"hello","client":"fcitx5-anime","protocol_version":1,"session_id":"wl-1"})",
           "encodeHello with session_id");

    const uint64_t big_seq = (1ULL << 58) + 777ULL;
    expect(encodeKeyChar(big_seq, "q", 0) ==
               std::string(R"({"type":"key","seq":)") + std::to_string(big_seq) +
                   R"(,"key":"char","text":"q"})",
           "encodeKeyChar 64-bit seq");
    expect(!encodeKeyChar(1, "ab", 0).has_value(), "encodeKeyChar rejects multi-char");
    expect(!encodeKeyChar(1, "", 0).has_value(), "encodeKeyChar rejects empty");
    expect(encodeKeyChar(1, "\"", 2) ==
               R"({"type":"key","seq":1,"key":"char","text":"\"","mods":2})",
           "encodeKeyChar escapes quote and mods");
    expect(encodeKeySpecial(2, "space") == R"({"type":"key","seq":2,"key":"space"})",
           "encodeKeySpecial space");
    expect(!encodeKeySpecial(2, "tab").has_value(), "encodeKeySpecial rejects invalid name");
    expect(encodeKeyDigit(3, 1) == R"({"type":"key","seq":3,"key":"digit","text":"1"})",
           "encodeKeyDigit 1");
    expect(!encodeKeyDigit(3, 10).has_value(), "encodeKeyDigit rejects >9");
    expect(encodeSelect(4, 0) == R"({"type":"select","seq":4,"index":0})", "encodeSelect");
    expect(encodeContext("Hi\n\"bob\"", "end\\") ==
               R"({"type":"context","before":"Hi\n\"bob\"","after":"end\\"})",
           "encodeContext escapes");
    expect(encodePrivacy(true) == R"({"type":"privacy","enabled":true})", "encodePrivacy");
    expect(encodeReset(5) == R"({"type":"reset","seq":5})", "encodeReset");
    expect(encodeConfig(EngineConfig{}) ==
               R"({"type":"config","space_commits":false,"smart_spacing":true,"max_candidates":9,"enable_phonetic":true,"enable_neural":true})",
           "encodeConfig default");
    expect(encodePing() == R"({"type":"ping"})", "encodePing");
    expect(encodePing(99) == R"({"type":"ping","id":99})", "encodePing with id");

    // 响应解码测试
    {
        auto r = decodeResponseLine(
            R"({"type":"hello_ack","protocol_version":1,"version":"0.1.0","capabilities":["l1-fuzzy","l2-ngram"]})");
        expect(r.has_value(), "decode hello_ack");
        const auto *ack = std::get_if<HelloAck>(&*r);
        expect(ack && ack->protocol_version == 1 && ack->version == "0.1.0" &&
                   ack->capabilities.size() == 2 && ack->capabilities[1] == "l2-ngram",
               "hello_ack fields");
    }
    {
        std::string line =
            std::string(R"({"type":"state","seq":)") + std::to_string(big_seq) +
            R"(,"revision":1,"preedit":"appie","cursor":5,"candidates":[{"text":"apple","kind":"TypoCorrection","comment":"typo \uD83D\uDE00","score":-1.25,"consumed_chars":5,"trailing_space":true},{"text":"next word","kind":"Continuation","comment":null,"score":-0.5,"consumed_chars":5,"trailing_space":true},{"text":"appie","kind":"Raw","score":-999,"consumed_chars":5,"trailing_space":false}],"passthrough":false,"final_l3":true})";
        auto r = decodeResponseLine(line);
        expect(r.has_value(), "decode state");
        const auto *st = std::get_if<PreeditState>(&*r);
        expect(st && st->seq == big_seq && st->revision == 1 && st->final_l3 &&
                   st->candidates.size() == 3,
               "state 64-bit seq & candidates");
        expect(st->candidates[0].comment == std::string("typo \xF0\x9F\x98\x80"),
               "utf16 surrogate decoded");
        expect(!st->candidates[1].comment.has_value(), "null comment -> nullopt");
        expect(st->candidates[2].kind == CandidateKind::Raw && !st->candidates[2].comment.has_value(),
               "missing comment -> nullopt");
    }
    {
        auto r = decodeResponseLine(
            R"({"type":"commit","seq":43,"text":".","delete_before":1,"remaining_preedit":"","candidates":[]})");
        expect(r.has_value(), "decode commit");
        const auto *cm = std::get_if<CommitAction>(&*r);
        expect(cm && cm->seq == 43 && cm->delete_before == 1 && cm->text == ".", "commit fields");
    }
    {
        auto r = decodeResponseLine(R"({"type":"pong","version":"0.1.0"})");
        expect(r.has_value(), "decode phase0 pong");
        const auto *p = std::get_if<Pong>(&*r);
        expect(p && p->version == "0.1.0" && !p->id.has_value(), "pong fields");
    }
    {
        auto r = decodeResponseLine(
            R"({"type":"error","seq":null,"code":"invalid_json","message":"bad"})");
        expect(r.has_value(), "decode error");
        const auto *e = std::get_if<ErrorMessage>(&*r);
        expect(e && !e->seq.has_value() && e->code == "invalid_json", "error fields");
    }

    // 异常/超限拒绝
    std::string huge = R"({"type":"pong","version":")" + std::string(kMaxIpcLineBytes + 10, 'a') +
                       R"("})";
    expect(!decodeResponseLine(huge).has_value(), "reject >64KiB line");
    expect(!decodeResponseLine("{\"type\":\"pong\",\n\"version\":\"0.1.0\"}").has_value(),
           "reject unescaped newline");
    expect(!decodeResponseLine(R"({"type":"state","seq":1})").has_value(), "reject missing fields");
    expect(!decodeResponseLine(R"({"type":"wat"})").has_value(), "reject unknown type");
    std::string deep = std::string(40, '[') + "1" + std::string(40, ']');
    expect(!decodeResponseLine(deep).has_value(), "reject deep nesting");
}

void testSessionStateAntiStalenessAndConcurrency() {
    SessionState st;
    const uint64_t s1 = st.nextSeq();
    st.onLocalChar('a');
    const uint64_t s2 = st.nextSeq();
    st.onLocalChar('p');
    expect(s1 == 1 && s2 == 2 && st.effectivePreedit() == "ap", "optimistic preedit");

    // 旧 seq=1 的响应必须严格丢弃
    PreeditState stale_seq;
    stale_seq.seq = s1;
    stale_seq.revision = 0;
    stale_seq.preedit = "a";
    stale_seq.cursor = 1;
    expect(!st.acceptState(stale_seq), "drop stale seq < last_sent_seq");

    // 当前 seq=2 rev=0 接受
    PreeditState curr_rev0;
    curr_rev0.seq = s2;
    curr_rev0.revision = 0;
    curr_rev0.preedit = "ap";
    curr_rev0.cursor = 2;
    curr_rev0.final_l3 = false;
    curr_rev0.candidates.push_back(Candidate{"apple", CandidateKind::Completion, std::nullopt, -1.0F, 2, true});
    expect(st.acceptState(curr_rev0), "accept rev 0");

    // 同 seq 重复 rev=0 丢弃
    expect(!st.acceptState(curr_rev0), "drop duplicate rev 0");

    // 同 seq 异步 L3 rev=1 覆盖成功
    PreeditState curr_rev1 = curr_rev0;
    curr_rev1.revision = 1;
    curr_rev1.final_l3 = true;
    curr_rev1.candidates.insert(
        curr_rev1.candidates.begin(),
        Candidate{"april", CandidateKind::Continuation, "\xE2\x9C\xA6", -0.5F, 2, true});
    expect(st.acceptState(curr_rev1), "accept higher revision 1");
    expect(st.lastAppliedRevision() == 1 && st.candidates().size() == 2, "rev 1 applied");

    // 快速连击：按选词 seq=3 后极快敲 'h' (seq=4)，随后晚到的 commit(seq=3) 不能丢弃也不覆盖新面板
    const uint64_t s_sel = st.nextSeq();
    st.onLocalSelect(0);
    const uint64_t s_next = st.nextSeq();
    st.onLocalChar('h');
    expect(st.effectivePreedit() == "h", "next word preedit after fast select");

    CommitAction commit_sel;
    commit_sel.seq = s_sel;
    commit_sel.text = "april ";
    commit_sel.delete_before = 0;
    commit_sel.remaining_preedit = "";
    const auto res_sel = st.acceptCommit(commit_sel);
    expect(res_sel.apply_commit && !res_sel.update_panel,
           "fast typing commit applies text without clobbering newer preedit");
    expect(st.effectivePreedit() == "h", "new preedit preserved");

    // reset 之后晚到的旧 commit 严格丢弃
    const uint64_t s_reset = st.nextSeq();
    st.onLocalReset(s_reset);
    CommitAction stale_after_reset;
    stale_after_reset.seq = s_next;
    stale_after_reset.text = "hello ";
    const auto res_reset = st.acceptCommit(stale_after_reset);
    expect(!res_reset.apply_commit && !res_reset.update_panel,
           "drop commit older than reset_seq");

    // 正常选词上屏附带尾空格时，触发 lastAutoSpace 记忆
    const uint64_t s_w = st.nextSeq();
    st.onLocalChar('h');
    const uint64_t s_pick = st.nextSeq();
    st.onLocalSelect(0);
    CommitAction normal_commit;
    normal_commit.seq = s_pick;
    normal_commit.text = "hello ";
    normal_commit.delete_before = 0;
    normal_commit.remaining_preedit = "";
    (void)s_w;
    const auto res_pick = st.acceptCommit(normal_commit);
    expect(res_pick.apply_commit && res_pick.update_panel && st.lastAutoSpace(),
           "auto trailing space tracked");

    // Enter 原样上屏哪怕尾随空格也不应误开启 lastAutoSpace
    const uint64_t s_ent = st.nextSeq();
    st.onLocalEnter();
    CommitAction enter_commit{s_ent, "foo ", 0, "", {}};
    const auto res_ent = st.acceptCommit(enter_commit);
    expect(res_ent.apply_commit && !st.lastAutoSpace(), "Enter commit does not set lastAutoSpace");
}

void testSurroundingUtf8Truncation() {
    // 构造多字节中英 + Emoji 混合文本，测试 before <= 256 / after <= 64 且永不截断半个码点
    std::string text;
    for (int i = 0; i < 300; ++i) {
        text.append((i % 2 == 0) ? "中" : "\xF0\x9F\x98\x80");
    }
    const size_t total_chars = 300;
    const size_t cursor = 280;
    auto slice = extractSurroundingContext(text, cursor, cursor, 256, 64);
    expect(isValidUtf8(slice.before), "before slice valid UTF-8");
    expect(isValidUtf8(slice.after), "after slice valid UTF-8");
    expect(utf8CharOffsetToByteOffset(slice.before, 9999) == slice.before.size(), "valid offset");

    // 验证确切字符数：before 截断到 256 码点，after 为剩余 20 码点
    size_t before_chars = 0;
    for (size_t i = 0; i < slice.before.size(); ++i) {
        if ((static_cast<unsigned char>(slice.before[i]) & 0xC0U) != 0x80U) {
            ++before_chars;
        }
    }
    size_t after_chars = 0;
    for (size_t i = 0; i < slice.after.size(); ++i) {
        if ((static_cast<unsigned char>(slice.after[i]) & 0xC0U) != 0x80U) {
            ++after_chars;
        }
    }
    expect(before_chars == 256, "before truncated to 256 unicode chars");
    expect(after_chars == total_chars - cursor, "after contains remaining 20 chars");

    // 存在选区 [anchor=10, cursor=20] 时，before 在 10 前、after 从 20 起
    auto sel_slice = extractSurroundingContext("0123456789SELECTED__abcdefgh", 20, 10, 256, 64);
    expect(sel_slice.before == "0123456789", "selection skips selected range for before");
    expect(sel_slice.after == "abcdefgh", "selection skips selected range for after");

    // 含非法 UTF-8 字节时不崩溃且返回合法 UTF-8
    std::string broken = "hello\xFFworld";
    auto broken_slice = extractSurroundingContext(broken, 6, 6, 256, 64);
    expect(isValidUtf8(broken_slice.before) && isValidUtf8(broken_slice.after),
           "broken UTF-8 sanitized cleanly");
}

void testShiftTapStateMachine() {
    ShiftTapDetector d;
    constexpr uint32_t kShiftL = 0xffe1;
    constexpr uint32_t kShiftR = 0xffe2;
    constexpr uint32_t kKeyA = 0x0041;

    // 正常单击左 Shift
    d.onKeyPress(kShiftL, true, false, false);
    expect(d.onKeyRelease(kShiftL, true, false), "single Shift_L tap toggles");
    expect(!d.onKeyRelease(kShiftL, true, false), "second release does not toggle");

    // Shift+A 不触发
    d.onKeyPress(kShiftL, true, false, false);
    d.onKeyPress(kKeyA, false, false, false);
    expect(!d.onKeyRelease(kShiftL, true, false), "Shift+A cancels tap");

    // 长按 Repeat 不触发
    d.onKeyPress(kShiftR, true, false, false);
    d.onKeyPress(kShiftR, true, false, true);
    expect(!d.onKeyRelease(kShiftR, true, false), "Shift auto-repeat cancels tap");

    // Ctrl+Shift 不触发
    d.onKeyPress(kShiftL, true, true, false);
    expect(!d.onKeyRelease(kShiftL, true, false), "Ctrl+Shift cancels tap");
}

void testSocketPermissionsAndClientFraming() {
    char dir_tmpl[] = "/tmp/anime-cpp-test-XXXXXX";
    char *dir = ::mkdtemp(dir_tmpl);
    expect(dir != nullptr, "create temp socket dir");
    ::chmod(dir, 0700);

    const std::string sock_path = std::string(dir) + "/engined.sock";
    const int srv = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    expect(srv >= 0, "create unix server socket");

    sockaddr_un addr{};
    addr.sun_family = AF_UNIX;
    std::strncpy(addr.sun_path, sock_path.c_str(), sizeof(addr.sun_path) - 1);
    expect(::bind(srv, reinterpret_cast<const sockaddr *>(&addr), sizeof(addr)) == 0,
           "bind unix server socket");
    expect(::listen(srv, 4) == 0, "listen unix server socket");

    // 1. 测试错误权限 0644 被拒绝
    expect(::chmod(sock_path.c_str(), 0644) == 0, "chmod 0644");
    std::string err;
    expect(!validateSocketPermissions(sock_path, &err), "reject 0644 socket");

    // 2. 改为 0600 并测目录权限异常拒绝
    expect(::chmod(sock_path.c_str(), 0600) == 0, "chmod 0600");
    expect(::chmod(dir, 0755) == 0, "chmod dir 0755");
    expect(!validateSocketPermissions(sock_path, &err), "reject 0755 dir");
    expect(::chmod(dir, 0700) == 0, "restore dir 0700");
    expect(validateSocketPermissions(sock_path, &err), "accept 0700 dir + 0600 sock");

    // 3. 拉起 mock 服务端，发送超长 >64 KiB 脏行后跟正常 pong，验证防内存爆炸 + 继续可用
    std::atomic<bool> done{false};
    std::thread server_thread([&]() {
        const int cli = ::accept(srv, nullptr, nullptr);
        if (cli >= 0) {
            char req_buf[1024];
            const ssize_t r = ::read(cli, req_buf, sizeof(req_buf));
            (void)r;
            std::string oversized =
                R"({"type":"pong","version":")" + std::string(kMaxIpcLineBytes + 200, 'x') +
                "\"}\n";
            std::string valid = R"({"type":"pong","version":"0.1.0","id":7})"
                                "\n";
            const std::string payload = oversized + valid;
            size_t sent = 0;
            while (sent < payload.size()) {
                const ssize_t n =
                    ::write(cli, payload.data() + sent, payload.size() - sent);
                if (n <= 0) break;
                sent += static_cast<size_t>(n);
            }
            while (!done.load()) {
                std::this_thread::sleep_for(std::chrono::milliseconds(5));
            }
            ::close(cli);
        }
    });

    IpcClient client;
    expect(client.connect(sock_path, &err), "client connect mock 0600 socket");
    if (client.isConnecting()) {
        expect(client.pollOnce(500, [](const IpcResponse &) {}), "complete connect");
    }
    expect(client.enqueueLine(*encodePing(7)), "send ping");

    std::vector<IpcResponse> received;
    for (int i = 0; i < 40 && received.empty(); ++i) {
        expect(client.pollOnce(50, [&](const IpcResponse &resp) { received.push_back(resp); }),
               "pollOnce succeeds even after oversized frame");
    }
    expect(received.size() == 1, "received valid pong after oversized drop");
    const auto *pong = std::get_if<Pong>(&received[0]);
    expect(pong && pong->id == 7 && pong->version == "0.1.0", "decoded follow-up pong");

    done.store(true);
    server_thread.join();
    client.disconnect();
    ::close(srv);
    ::unlink(sock_path.c_str());
    ::rmdir(dir);
}

int runLiveSocketE2E(const std::string &sock_path) {
    IpcClient client;
    std::string err;
    expect(client.connect(sock_path, &err), err.c_str());
    if (client.isConnecting()) {
        expect(client.pollOnce(500, [](const IpcResponse &) {}), "live connect writable");
    }
    expect(client.isConnected(), "live client connected");

    auto send_and_wait = [&](const std::string &line, auto pred) -> IpcResponse {
        expect(client.enqueueLine(line), "enqueue live line");
        std::optional<IpcResponse> matched;
        for (int attempt = 0; attempt < 50 && !matched; ++attempt) {
            expect(client.pollOnce(100, [&](const IpcResponse &r) {
                       if (pred(r)) {
                           matched = r;
                       }
                   }),
                   "live pollOnce");
        }
        expect(matched.has_value(), "expected response from animed");
        return *matched;
    };

    // 1. hello -> hello_ack
    auto ack_resp = send_and_wait(*encodeHello("fcitx5-anime-test", 1), [](const IpcResponse &r) {
        return std::holds_alternative<HelloAck>(r);
    });
    const auto &ack = std::get<HelloAck>(ack_resp);
    expect(ack.protocol_version == 1, "live hello_ack proto=1");

    // 2. config + context + privacy(false) + 键入 "i wnt appie"
    expect(client.enqueueLine(*encodeConfig(EngineConfig{})), "send config");
    expect(client.enqueueLine(*encodeContext("Thank you for your ", "")), "send context");
    expect(client.enqueueLine(*encodePrivacy(false)), "send privacy(false)");

    SessionState st;
    const std::string phrase = "i wnt appie";
    std::optional<PreeditState> last_state;
    for (char c : phrase) {
        const uint64_t seq = st.nextSeq();
        std::string req;
        if (c == ' ') {
            st.onLocalSpace(false);
            req = *encodeKeySpecial(seq, "space");
        } else {
            st.onLocalChar(c);
            req = *encodeKeyChar(seq, std::string_view(&c, 1));
        }
        auto resp = send_and_wait(req, [seq](const IpcResponse &r) {
            const auto *s = std::get_if<PreeditState>(&r);
            return s && s->seq == seq;
        });
        const auto &ps = std::get<PreeditState>(resp);
        expect(st.acceptState(ps), "accept live preedit state");
        last_state = ps;
    }
    expect(last_state.has_value() && last_state->preedit == phrase &&
               !last_state->candidates.empty(),
           "live space-in-preedit produced candidates");

    // 确认末尾至少保留 Raw 兜底候选
    bool has_raw_or_sentence = false;
    for (const auto &c : last_state->candidates) {
        if (c.kind == CandidateKind::Raw || c.kind == CandidateKind::Sentence) {
            has_raw_or_sentence = true;
            break;
        }
    }
    expect(has_raw_or_sentence, "live candidates include Raw/Sentence");

    // 3. 选第一候选上屏
    const uint64_t sel_seq = st.nextSeq();
    st.onLocalSelect(0);
    auto cm_resp = send_and_wait(*encodeSelect(sel_seq, 0), [sel_seq](const IpcResponse &r) {
        const auto *cm = std::get_if<CommitAction>(&r);
        return cm && cm->seq == sel_seq;
    });
    const auto &cm = std::get<CommitAction>(cm_resp);
    auto sel_res = st.acceptCommit(cm);
    expect(sel_res.apply_commit && !cm.text.empty(), "live select committed");

    // 若整句已上屏带尾空格，验证紧接 '.' 触发 delete_before == 1
    if (st.lastAutoSpace()) {
        const uint64_t p_seq = st.nextSeq();
        st.onLocalClosingPunct();
        auto punct_resp = send_and_wait(
            *encodeKeyChar(p_seq, "."), [p_seq](const IpcResponse &r) {
                const auto *c = std::get_if<CommitAction>(&r);
                return c && c->seq == p_seq;
            });
        const auto &pcm = std::get<CommitAction>(punct_resp);
        expect(pcm.delete_before == 1 && pcm.text == ".", "live smart punct delete_before=1");
        (void)st.acceptCommit(pcm);
    }

    // 4. 输入 "foobar" 后 Enter 原样上屏
    for (char c : std::string_view("foobar")) {
        const uint64_t seq = st.nextSeq();
        st.onLocalChar(c);
        auto r = send_and_wait(*encodeKeyChar(seq, std::string_view(&c, 1)),
                               [seq](const IpcResponse &resp) {
                                   const auto *s = std::get_if<PreeditState>(&resp);
                                   return s && s->seq == seq;
                               });
        expect(st.acceptState(std::get<PreeditState>(r)), "accept foobar char");
    }
    const uint64_t ent_seq = st.nextSeq();
    st.onLocalEnter();
    auto ent_resp = send_and_wait(
        *encodeKeySpecial(ent_seq, "enter"), [ent_seq](const IpcResponse &resp) {
            const auto *c = std::get_if<CommitAction>(&resp);
            return c && c->seq == ent_seq;
        });
    expect(std::get<CommitAction>(ent_resp).text == "foobar", "live Enter commits raw foobar");

    // 5. Reset 清理验证
    const uint64_t rst_seq = st.nextSeq();
    st.onLocalReset(rst_seq);
    auto rst_resp = send_and_wait(*encodeReset(rst_seq), [rst_seq](const IpcResponse &resp) {
        const auto *s = std::get_if<PreeditState>(&resp);
        return s && s->seq == rst_seq;
    });
    expect(std::get<PreeditState>(rst_resp).preedit.empty(), "live reset clears preedit");

    std::cout << "LIVE SOCKET E2E OK: hello_ack(v" << ack.version << "), preedit='" << phrase
              << "', commit='" << cm.text << "'\n";
    return 0;
}

}  // namespace

int main(int argc, char **argv) {
    if (argc == 3 && std::string_view(argv[1]) == "--live-socket") {
        return runLiveSocketE2E(argv[2]);
    }
    testProtocolCodec();
    testSessionStateAntiStalenessAndConcurrency();
    testSurroundingUtf8Truncation();
    testShiftTapStateMachine();
    testSocketPermissionsAndClientFraming();
    std::cout << "ALL FCITX5 FRONTEND UNIT TESTS PASSED\n";
    return 0;
}
