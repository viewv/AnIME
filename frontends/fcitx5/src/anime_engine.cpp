// fcitx5-anime — AnIME 的 Fcitx5 C++20 薄前端插件
//
// 职责严格限定为：按键拦截/透传判定、NDJSON Unix Socket 异步 IPC、
// seq/revision 防过期面板刷新、Surrounding Text 采集与 Password/Sensitive 隐私隔离。
// 严禁在此文件内实现任何词典查找、纠错算法或语言模型计算。

#include "anime_candidate.h"
#include "anime_config.h"
#include "ipc_client.h"
#include "ipc_protocol.h"
#include "session_state.h"

#include <fcitx/addonfactory.h>
#include <fcitx/addoninstance.h>
#include <fcitx/addonmanager.h>
#include <fcitx/event.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputcontextmanager.h>
#include <fcitx/inputcontextproperty.h>
#include <fcitx/inputmethodengine.h>
#include <fcitx/inputpanel.h>
#include <fcitx/instance.h>
#include <fcitx/surroundingtext.h>
#include <fcitx/text.h>
#include <fcitx-config/iniparser.h>
#include <fcitx-utils/capabilityflags.h>
#include <fcitx-utils/event.h>
#include <fcitx-utils/eventloopinterface.h>
#include <fcitx-utils/key.h>
#include <fcitx-utils/keysym.h>
#include <fcitx-utils/log.h>
#include <fcitx-utils/standardpaths.h>

#include <algorithm>
#include <ctime>
#include <memory>
#include <optional>
#include <string>
#include <vector>

namespace {

FCITX_DEFINE_LOG_CATEGORY(anime_log, "anime");

constexpr uint64_t kInitialReconnectBackoffUsec = 500ULL * 1000ULL;
constexpr uint64_t kMaxReconnectBackoffUsec = 5ULL * 1000ULL * 1000ULL;
constexpr uint64_t kActivationRetryCooldownUsec = 250ULL * 1000ULL;

struct AnimeContextState final : public fcitx::InputContextProperty {
    bool passthrough_mode = false;
    anime::ShiftTapDetector shift_detector;

    bool needCopy() const override { return true; }

    void copyTo(fcitx::InputContextProperty *other) override {
        if (auto *dst = dynamic_cast<AnimeContextState *>(other)) {
            dst->passthrough_mode = passthrough_mode;
        }
    }
};

class AnimeEngine final : public fcitx::InputMethodEngineV2 {
public:
    explicit AnimeEngine(fcitx::Instance *instance)
        : instance_(instance),
          factory_([](fcitx::InputContext &) { return new AnimeContextState(); }) {
        instance_->inputContextManager().registerProperty("animeState", &factory_);

        event_watchers_.push_back(instance_->watchEvent(
            fcitx::EventType::InputContextCapabilityChanged,
            fcitx::EventWatcherPhase::Default,
            [this](fcitx::Event &event) {
                auto &cap_event = static_cast<fcitx::CapabilityChangedEvent &>(event);
                auto *ic = cap_event.inputContext();
                if (!ic || ic != active_ic_.get()) {
                    return;
                }
                if (ic->capabilityFlags().test(fcitx::CapabilityFlag::Password)) {
                    clearIcPanel(ic);
                    if (session_.isComposing()) {
                        sendResetForIc(ic);
                    }
                }
                syncPrivacy(ic);
            }));

        event_watchers_.push_back(instance_->watchEvent(
            fcitx::EventType::InputContextSurroundingTextUpdated,
            fcitx::EventWatcherPhase::Default,
            [this](fcitx::Event &event) {
                auto &sur_event = static_cast<fcitx::SurroundingTextUpdatedEvent &>(event);
                auto *ic = sur_event.inputContext();
                if (!ic || ic != active_ic_.get()) {
                    return;
                }
                // 仅在空闲阶段主动刷新 surrounding text，避免 preedit 波动造成无谓 IPC。
                if (!session_.isComposing()) {
                    syncContext(ic);
                }
            }));

        reloadConfig();
        tryConnect();
    }

    const fcitx::Configuration *getConfig() const override { return &addon_config_; }

    void setConfig(const fcitx::RawConfig &config) override {
        addon_config_.load(config, true);
        fcitx::safeSaveAsIni(addon_config_, fcitx::StandardPathsType::PkgConfig, "conf/anime.conf");
        applyConfigToEngine();
    }

    void reloadConfig() override {
        fcitx::readAsIni(addon_config_, fcitx::StandardPathsType::PkgConfig, "conf/anime.conf");
        applyConfigToEngine();
    }

    ~AnimeEngine() override {
        event_watchers_.clear();
        defer_cleanup_.reset();
        stale_io_events_.clear();
        io_event_.reset();
        reconnect_timer_.reset();
        factory_.unregister();
        ipc_.disconnect();
    }

    void activate(const fcitx::InputMethodEntry &, fcitx::InputContextEvent &event) override {
        auto *ic = event.inputContext();
        if (!ic) {
            return;
        }
        active_ic_ = ic->watch();
        if (auto *state = ic->propertyFor(&factory_)) {
            state->shift_detector.reset();
        }
        if (!ipc_.isConnected() && !ipc_.isConnecting()) {
            const uint64_t now_us = fcitx::now(CLOCK_MONOTONIC);
            if (now_us >= last_connect_attempt_usec_ + kActivationRetryCooldownUsec) {
                tryConnect();
            }
        }
        if (ipc_.isConnected()) {
            syncPrivacy(ic);
            syncContext(ic);
        }
        ic->updateUserInterface(fcitx::UserInterfaceComponent::StatusArea);
    }

    void deactivate(const fcitx::InputMethodEntry &entry,
                    fcitx::InputContextEvent &event) override {
        reset(entry, event);
        if (auto *ic = event.inputContext()) {
            if (ic == active_ic_.get()) {
                active_ic_.unwatch();
            }
        }
    }

    void reset(const fcitx::InputMethodEntry &, fcitx::InputContextEvent &event) override {
        auto *ic = event.inputContext();
        if (!ic) {
            return;
        }
        if (auto *state = ic->propertyFor(&factory_)) {
            state->shift_detector.reset();
        }
        clearIcPanel(ic);
        if (ic == active_ic_.get()) {
            sendResetForIc(ic);
        }
    }

    std::string subMode(const fcitx::InputMethodEntry &, fcitx::InputContext &ic) override {
        if (auto *state = ic.propertyFor(&factory_)) {
            if (state->passthrough_mode) {
                return "Raw";
            }
        }
        return "En";
    }

    std::string subModeLabelImpl(const fcitx::InputMethodEntry &entry,
                                 fcitx::InputContext &ic) override {
        return subMode(entry, ic);
    }

    void keyEvent(const fcitx::InputMethodEntry &, fcitx::KeyEvent &keyEvent) override {
        auto *ic = keyEvent.inputContext();
        if (!ic) {
            return;
        }
        if (auto *prev_ic = active_ic_.get(); prev_ic != ic) {
            if (prev_ic && session_.isComposing()) {
                clearIcPanel(prev_ic);
                sendResetForIc(prev_ic);
            }
            active_ic_ = ic->watch();
        }

        const auto caps = ic->capabilityFlags();
        if (caps.test(fcitx::CapabilityFlag::Disable)) {
            return;
        }

        auto *state = ic->propertyFor(&factory_);
        if (!state) {
            return;
        }

        const fcitx::Key raw_key = keyEvent.rawKey();
        const fcitx::Key norm_key = keyEvent.key();
        const uint32_t raw_sym = static_cast<uint32_t>(raw_key.sym());
        const bool is_shift_sym =
            (raw_sym == FcitxKey_Shift_L || raw_sym == FcitxKey_Shift_R);
        const auto raw_mods = raw_key.states();
        const fcitx::KeyStates kNonShiftModMask{
            fcitx::KeyState::Ctrl,
            fcitx::KeyState::Alt,
            fcitx::KeyState::Super,
            fcitx::KeyState::Super2,
            fcitx::KeyState::Hyper,
            fcitx::KeyState::Meta,
        };
        const bool has_non_shift_mods = raw_mods.testAny(kNonShiftModMask);
        const bool is_repeat = raw_mods.test(fcitx::KeyState::Repeat);

        if (!keyEvent.isRelease()) {
            state->shift_detector.onKeyPress(raw_sym, is_shift_sym, has_non_shift_mods, is_repeat);
            if (is_shift_sym) {
                return;
            }
        } else {
            if (state->shift_detector.onKeyRelease(raw_sym, is_shift_sym, has_non_shift_mods)) {
                togglePassthrough(ic, *state);
                keyEvent.filterAndAccept();
                return;
            }
            return;
        }

        // 密码框强制不拦截、不组合，且通知 daemon 开启隐私屏蔽与清空上文缓存。
        if (caps.test(fcitx::CapabilityFlag::Password)) {
            if (session_.isComposing()) {
                clearIcPanel(ic);
                sendResetForIc(ic);
            }
            syncPrivacy(ic);
            syncContext(ic);
            return;
        }

        if (state->passthrough_mode) {
            return;
        }

        // 守护进程离线时优雅 fail-open：绝不锁死用户键盘，并尝试静默重连。
        if (!ipc_.isConnected()) {
            if (!ipc_.isConnecting()) {
                const uint64_t now_us = fcitx::now(CLOCK_MONOTONIC);
                if (now_us >= last_connect_attempt_usec_ + kMaxReconnectBackoffUsec) {
                    tryConnect();
                }
            }
            return;
        }

        // Ctrl/Alt/Super 快捷键一律原样放行给桌面或宿主应用。
        if (norm_key.states().testAny(kNonShiftModMask)) {
            return;
        }

        syncPrivacy(ic);
        if (!session_.isComposing()) {
            syncContext(ic);
        }

        const uint32_t mods = static_cast<uint32_t>(raw_mods.toInteger() & 0xFFFFU);
        const uint32_t sym = static_cast<uint32_t>(norm_key.sym());
        const uint32_t unicode = fcitx::Key::keySymToUnicode(norm_key.sym());

        if (session_.isComposing()) {
            if (sym == FcitxKey_space) {
                const uint64_t seq = session_.nextSeq();
                auto line = anime::ipc::encodeKeySpecial(seq, "space", mods);
                if (line && sendLine(*line)) {
                    session_.onLocalSpace(config_.space_commits);
                    keyEvent.filterAndAccept();
                }
                return;
            }
            if (sym == FcitxKey_BackSpace) {
                const uint64_t seq = session_.nextSeq();
                auto line = anime::ipc::encodeKeySpecial(seq, "backspace", mods);
                if (line && sendLine(*line)) {
                    session_.onLocalBackspace();
                    if (!session_.isComposing()) {
                        clearIcPanel(ic);
                    }
                    keyEvent.filterAndAccept();
                }
                return;
            }
            if (sym == FcitxKey_Return || sym == FcitxKey_KP_Enter) {
                const uint64_t seq = session_.nextSeq();
                auto line = anime::ipc::encodeKeySpecial(seq, "enter", mods);
                if (line && sendLine(*line)) {
                    session_.onLocalEnter();
                    clearIcPanel(ic);
                    keyEvent.filterAndAccept();
                }
                return;
            }
            if (sym == FcitxKey_Escape) {
                const uint64_t seq = session_.nextSeq();
                auto line = anime::ipc::encodeKeySpecial(seq, "escape", mods);
                if (line && sendLine(*line)) {
                    session_.onLocalEscape();
                    clearIcPanel(ic);
                    keyEvent.filterAndAccept();
                }
                return;
            }
            if (sym == FcitxKey_Page_Down || sym == FcitxKey_Page_Up) {
                if (auto cand_list = ic->inputPanel().candidateList()) {
                    if (auto *pageable = cand_list->toPageable();
                        pageable && (pageable->hasPrev() || pageable->hasNext())) {
                        if (sym == FcitxKey_Page_Down && pageable->hasNext()) {
                            pageable->next();
                        } else if (sym == FcitxKey_Page_Up && pageable->hasPrev()) {
                            pageable->prev();
                        }
                        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
                        keyEvent.filterAndAccept();
                        return;
                    }
                }
            }
            if (sym == FcitxKey_Down || sym == FcitxKey_Up) {
                if (auto cand_list = ic->inputPanel().candidateList()) {
                    if (auto *movable = cand_list->toCursorMovable()) {
                        if (sym == FcitxKey_Down) {
                            movable->nextCandidate();
                        } else {
                            movable->prevCandidate();
                        }
                        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
                        keyEvent.filterAndAccept();
                        return;
                    }
                }
            }
            if (sym >= FcitxKey_1 && sym <= FcitxKey_9 && !raw_key.isKeyPad() &&
                !norm_key.states().test(fcitx::KeyState::Shift)) {
                const auto digit = static_cast<uint8_t>(sym - FcitxKey_0);
                const size_t local_idx = static_cast<size_t>(digit - 1U);
                const bool has_inflight_state =
                    session_.lastAppliedSeq() < session_.lastSentSeq();
                auto cand_list = ic->inputPanel().candidateList();
                if (!has_inflight_state && cand_list) {
                    if (const auto *common =
                            dynamic_cast<const fcitx::CommonCandidateList *>(cand_list.get())) {
                        const int cur_size = common->size();
                        if (local_idx >= static_cast<size_t>(std::max(0, cur_size))) {
                            keyEvent.filterAndAccept();
                            return;
                        }
                        const int page = std::max(0, common->currentPage());
                        const int page_size = std::max(1, common->pageSize());
                        const size_t global_idx =
                            static_cast<size_t>(page) * static_cast<size_t>(page_size) + local_idx;
                        const uint64_t seq = session_.nextSeq();
                        auto line = anime::ipc::encodeSelect(seq, global_idx);
                        if (line && sendLine(*line)) {
                            session_.onLocalSelect(global_idx);
                            keyEvent.filterAndAccept();
                        }
                        return;
                    }
                }
                const uint64_t seq = session_.nextSeq();
                auto line = anime::ipc::encodeKeyDigit(seq, digit, mods);
                if (line && sendLine(*line)) {
                    // 组合态主键盘 1-9 一律消费（防快击首包未归或候选不足时数字漏进应用）；
                    // 仅当索引有效或仍有更早期状态在途时推进本地乐观预编辑。
                    if (local_idx < session_.candidates().size() || has_inflight_state) {
                        session_.onLocalSelect(local_idx);
                    }
                    keyEvent.filterAndAccept();
                }
                return;
            }
            if (anime::isComposingWordChar(unicode)) {
                const char ch = static_cast<char>(unicode);
                const std::string utf8_ch(1, ch);
                const uint64_t seq = session_.nextSeq();
                auto line = anime::ipc::encodeKeyChar(seq, utf8_ch, mods);
                if (line && sendLine(*line)) {
                    session_.onLocalChar(ch);
                    keyEvent.filterAndAccept();
                }
                return;
            }
            // 其他控制键或未纳入词内字符的符号直接透传。
            return;
        }

        // 空闲态仅对 ASCII 字母触发组合，或对智能尾空格后的终止标点触发回删吞空格。
        if (anime::isAsciiLetter(unicode)) {
            const char ch = static_cast<char>(unicode);
            const std::string utf8_ch(1, ch);
            const uint64_t seq = session_.nextSeq();
            auto line = anime::ipc::encodeKeyChar(seq, utf8_ch, mods);
            if (line && sendLine(*line)) {
                session_.onLocalChar(ch);
                keyEvent.filterAndAccept();
            }
            return;
        }

        if (config_.smart_spacing && session_.lastAutoSpace() &&
            anime::isClosingPunctuation(unicode)) {
            const char ch = static_cast<char>(unicode);
            const std::string utf8_ch(1, ch);
            const uint64_t seq = session_.nextSeq();
            auto line = anime::ipc::encodeKeyChar(seq, utf8_ch, mods);
            if (line && sendLine(*line)) {
                session_.onLocalClosingPunct();
                keyEvent.filterAndAccept();
            }
            return;
        }
    }

private:
    void togglePassthrough(fcitx::InputContext *ic, AnimeContextState &state) {
        state.passthrough_mode = !state.passthrough_mode;
        if (state.passthrough_mode && session_.isComposing()) {
            // 遵循 core-api.md §2.3：切入直通态时，先原样上屏已有预编辑串，不丢失用户输入。
            const std::string raw = session_.effectivePreedit();
            if (!raw.empty()) {
                ic->commitString(raw);
            }
            clearIcPanel(ic);
            sendResetForIc(ic);
        }
        ic->updateProperty(&factory_);
        ic->updateUserInterface(fcitx::UserInterfaceComponent::StatusArea);
        instance_->showInputMethodInformation(ic);
    }

    void sendResetForIc(fcitx::InputContext *ic) {
        if (!ipc_.isConnected()) {
            session_.onLocalReset(session_.lastSentSeq());
            return;
        }
        const uint64_t seq = session_.nextSeq();
        session_.onLocalReset(seq);
        if (auto line = anime::ipc::encodeReset(seq)) {
            if (!sendLine(*line)) {
                clearIcPanel(ic);
            }
        }
    }

    void syncPrivacy(fcitx::InputContext *ic) {
        if (!ipc_.isConnected() || !ic) {
            return;
        }
        const bool is_private =
            ic->capabilityFlags().testAny(fcitx::CapabilityFlag::PasswordOrSensitive);
        if (!session_.shouldSendPrivacy(is_private)) {
            return;
        }
        if (auto line = anime::ipc::encodePrivacy(is_private)) {
            if (sendLine(*line)) {
                session_.markPrivacySent(is_private);
            }
        }
    }

    void syncContext(fcitx::InputContext *ic) {
        if (!ipc_.isConnected() || !ic) {
            return;
        }
        const auto caps = ic->capabilityFlags();
        const auto &sur = ic->surroundingText();
        if (caps.test(fcitx::CapabilityFlag::Password) ||
            !caps.test(fcitx::CapabilityFlag::SurroundingText) || !sur.isValid()) {
            // 切换到密码框或不支持 surrounding text 的终端时主动清空引擎残留上下文。
            if (session_.shouldSendContext("", "")) {
                if (auto line = anime::ipc::encodeContext("", "")) {
                    if (sendLine(*line)) {
                        session_.markContextSent("", "");
                    }
                }
            }
            return;
        }
        auto slice = anime::extractSurroundingContext(sur.text(), sur.cursor(), sur.anchor());
        if (!session_.shouldSendContext(slice.before, slice.after)) {
            return;
        }
        if (auto line = anime::ipc::encodeContext(slice.before, slice.after)) {
            if (sendLine(*line)) {
                session_.markContextSent(std::move(slice.before), std::move(slice.after));
            }
        }
    }

    void selectCandidateByUi(fcitx::InputContext *ic, size_t index) {
        if (!ic || ic != active_ic_.get() || !ipc_.isConnected() || !session_.isComposing()) {
            return;
        }
        const uint64_t seq = session_.nextSeq();
        if (auto line = anime::ipc::encodeSelect(seq, index)) {
            if (sendLine(*line)) {
                session_.onLocalSelect(index);
            }
        }
    }

    void clearIcPanel(fcitx::InputContext *ic) {
        if (!ic) {
            return;
        }
        ic->inputPanel().reset();
        ic->updatePreedit();
        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
    }

    void renderIcPanel(fcitx::InputContext *ic,
                       const std::string &preedit,
                       size_t cursor,
                       const std::vector<anime::ipc::Candidate> &candidates) {
        if (!ic) {
            return;
        }
        if (preedit.empty()) {
            clearIcPanel(ic);
            return;
        }

        const bool use_client_preedit =
            ic->capabilityFlags().test(fcitx::CapabilityFlag::Preedit);
        fcitx::Text preedit_text;
        preedit_text.append(preedit, use_client_preedit ? fcitx::TextFormatFlag::Underline
                                                        : fcitx::TextFormatFlag::NoFlag);
        preedit_text.setCursor(
            static_cast<int>(anime::utf8CharOffsetToByteOffset(preedit, cursor)));

        if (use_client_preedit) {
            ic->inputPanel().setClientPreedit(preedit_text);
            ic->inputPanel().setPreedit(fcitx::Text());
        } else {
            ic->inputPanel().setClientPreedit(fcitx::Text());
            ic->inputPanel().setPreedit(preedit_text);
        }

        if (!candidates.empty()) {
            ic->inputPanel().setCandidateList(anime::buildCandidateList(
                preedit,
                candidates,
                ui_options_,
                [this](fcitx::InputContext *target_ic, size_t idx) {
                    selectCandidateByUi(target_ic, idx);
                }));
        } else {
            ic->inputPanel().setCandidateList(nullptr);
        }

        ic->updatePreedit();
        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
    }

    bool sendLine(const std::string &line) {
        if (!ipc_.enqueueLine(line)) {
            handleDisconnect();
            return false;
        }
        updateIoEventMask();
        return true;
    }

    void tryConnect() {
        last_connect_attempt_usec_ = fcitx::now(CLOCK_MONOTONIC);
        std::string err;
        const std::string sock_path = anime::ipc::resolveSocketPath();
        if (!ipc_.connect(sock_path, &err)) {
            FCITX_LOGC(anime_log, Debug) << "animed socket connect deferred: " << err;
            scheduleReconnect();
            return;
        }
        reconnect_backoff_usec_ = kInitialReconnectBackoffUsec;
        if (reconnect_timer_) {
            reconnect_timer_->setEnabled(false);
        }
        attachIoWatcher();
        if (ipc_.isConnected()) {
            onTransportConnected();
        }
    }

    void onTransportConnected() {
        if (auto hello = anime::ipc::encodeHello("fcitx5-anime", anime::ipc::kProtocolVersion)) {
            if (!sendLine(*hello)) {
                return;
            }
        }
        if (auto cfg = anime::ipc::encodeConfig(config_)) {
            if (!sendLine(*cfg)) {
                return;
            }
        }
        if (auto *ic = active_ic_.get()) {
            syncPrivacy(ic);
            syncContext(ic);
        }
    }

    void handleDisconnect() {
        // 先停用 IO 监听再 close(fd)，避免事件循环内部针对已关闭 fd 报 EBADF；
        // 若正处于 IO 回调栈帧内，把旧 EventSourceIO 移到 defer 阶段销毁以防自毁悬空。
        if (io_event_) {
            io_event_->setEnabled(false);
            if (in_io_callback_) {
                stale_io_events_.push_back(std::move(io_event_));
                if (!defer_cleanup_) {
                    defer_cleanup_ = instance_->eventLoop().addDeferEvent(
                        [this](fcitx::EventSource *source) {
                            source->setEnabled(false);
                            stale_io_events_.clear();
                            return true;
                        });
                } else {
                    defer_cleanup_->setEnabled(true);
                }
            } else {
                io_event_.reset();
            }
        }
        ipc_.disconnect();
        session_.onDisconnected();
        if (auto *ic = active_ic_.get()) {
            clearIcPanel(ic);
        }
        scheduleReconnect();
    }

    void attachIoWatcher() {
        if (io_event_) {
            io_event_->setEnabled(false);
            io_event_.reset();
        }
        if (ipc_.fd() < 0) {
            return;
        }
        io_event_ = instance_->eventLoop().addIOEvent(
            ipc_.fd(),
            desiredIoFlags(),
            [this](fcitx::EventSourceIO *, int, fcitx::IOEventFlags flags) {
                struct CallbackGuard {
                    bool &flag;
                    explicit CallbackGuard(bool &f) : flag(f) { flag = true; }
                    ~CallbackGuard() { flag = false; }
                } guard(in_io_callback_);

                if (flags.test(fcitx::IOEventFlag::Out)) {
                    const bool was_connecting = ipc_.isConnecting();
                    if (!ipc_.onWritable()) {
                        handleDisconnect();
                        return false;
                    }
                    if (was_connecting && ipc_.isConnected()) {
                        onTransportConnected();
                        if (ipc_.fd() < 0) {
                            return false;
                        }
                    }
                }
                if (flags.test(fcitx::IOEventFlag::In)) {
                    if (!ipc_.onReadable([this](const anime::ipc::IpcResponse &resp) {
                            handleIpcResponse(resp);
                        })) {
                        handleDisconnect();
                        return false;
                    }
                }
                const fcitx::IOEventFlags kErrOrHup{
                    fcitx::IOEventFlag::Err,
                    fcitx::IOEventFlag::Hup,
                };
                if (flags.testAny(kErrOrHup) && !flags.test(fcitx::IOEventFlag::In)) {
                    handleDisconnect();
                    return false;
                }
                updateIoEventMask();
                return true;
            });
    }

    fcitx::IOEventFlags desiredIoFlags() const {
        fcitx::IOEventFlags flags = {
            fcitx::IOEventFlag::In,
            fcitx::IOEventFlag::Err,
            fcitx::IOEventFlag::Hup,
        };
        if (ipc_.isConnecting() || ipc_.hasPendingWrite()) {
            flags |= fcitx::IOEventFlag::Out;
        }
        return flags;
    }

    void updateIoEventMask() {
        if (io_event_ && ipc_.fd() >= 0) {
            io_event_->setEvents(desiredIoFlags());
        }
    }

    void scheduleReconnect() {
        if (reconnect_timer_ && reconnect_timer_->isEnabled()) {
            return;
        }
        const uint64_t delay = reconnect_backoff_usec_;
        reconnect_backoff_usec_ =
            std::min<uint64_t>(reconnect_backoff_usec_ * uint64_t{2}, kMaxReconnectBackoffUsec);
        const uint64_t fire_at = fcitx::now(CLOCK_MONOTONIC) + delay;
        if (!reconnect_timer_) {
            reconnect_timer_ = instance_->eventLoop().addTimeEvent(
                CLOCK_MONOTONIC,
                fire_at,
                100ULL * 1000ULL,
                [this](fcitx::EventSourceTime *source, uint64_t) {
                    source->setEnabled(false);
                    tryConnect();
                    return true;
                });
            reconnect_timer_->setOneShot();
        } else {
            reconnect_timer_->setTime(fire_at);
            reconnect_timer_->setOneShot();
            reconnect_timer_->setEnabled(true);
        }
    }

    void handleIpcResponse(const anime::ipc::IpcResponse &resp) {
        if (const auto *ack = std::get_if<anime::ipc::HelloAck>(&resp)) {
            FCITX_LOGC(anime_log, Debug)
                << "Connected to animed " << ack->version << " (proto=" << ack->protocol_version
                << ")";
            return;
        }
        if (std::holds_alternative<anime::ipc::Pong>(resp)) {
            return;
        }
        if (const auto *err = std::get_if<anime::ipc::ErrorMessage>(&resp)) {
            FCITX_LOGC(anime_log, Warn)
                << "animed error [" << err->code << "]: " << err->message;
            return;
        }
        if (const auto *st = std::get_if<anime::ipc::PreeditState>(&resp)) {
            if (!session_.acceptState(*st)) {
                return;
            }
            auto *ic = active_ic_.get();
            if (!ic || !ic->hasFocus()) {
                return;
            }
            renderIcPanel(ic, st->preedit, st->cursor, st->candidates);
            return;
        }
        if (const auto *cm = std::get_if<anime::ipc::CommitAction>(&resp)) {
            const auto applied = session_.acceptCommit(*cm);
            if (!applied.apply_commit) {
                return;
            }
            auto *ic = active_ic_.get();
            if (!ic || !ic->hasFocus()) {
                return;
            }
            // Wayland/X11 客户端前置回删（如智能尾空格后紧跟 '.' 先删 1 个空格再上屏 '.'）
            if (cm->delete_before > 0) {
                const int offset = -static_cast<int>(cm->delete_before);
                const auto size = static_cast<unsigned int>(cm->delete_before);
                ic->deleteSurroundingText(offset, size);
            }
            if (!cm->text.empty()) {
                ic->commitString(cm->text);
            }
            if (applied.update_panel) {
                renderIcPanel(ic, cm->remaining_preedit, session_.confirmedCursor(), cm->candidates);
            }
        }
    }

    void applyConfigToEngine() {
        config_ = anime::toEngineConfig(addon_config_);
        ui_options_ = anime::toPresentationOptions(addon_config_);
        if (!config_.smart_spacing) {
            session_.clearLastAutoSpace();
        }
        if (ipc_.isConnected()) {
            if (auto cfg = anime::ipc::encodeConfig(config_)) {
                sendLine(*cfg);
            }
        }
        if (auto *ic = active_ic_.get(); ic && ic->hasFocus() && !session_.confirmedPreedit().empty()) {
            renderIcPanel(ic,
                          session_.confirmedPreedit(),
                          session_.confirmedCursor(),
                          session_.candidates());
        }
    }

    fcitx::Instance *instance_;
    fcitx::FactoryFor<AnimeContextState> factory_;
    fcitx::AnimeConfig addon_config_;
    anime::ipc::IpcClient ipc_;
    anime::SessionState session_;
    anime::ipc::EngineConfig config_;
    anime::CandidatePresentationOptions ui_options_;
    fcitx::TrackableObjectReference<fcitx::InputContext> active_ic_;
    std::unique_ptr<fcitx::EventSourceIO> io_event_;
    std::vector<std::unique_ptr<fcitx::EventSourceIO>> stale_io_events_;
    std::unique_ptr<fcitx::EventSource> defer_cleanup_;
    std::unique_ptr<fcitx::EventSourceTime> reconnect_timer_;
    bool in_io_callback_ = false;
    uint64_t reconnect_backoff_usec_ = kInitialReconnectBackoffUsec;
    uint64_t last_connect_attempt_usec_ = 0;
    std::vector<std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>>> event_watchers_;
};

class AnimeAddonFactory final : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override {
        return new AnimeEngine(manager->instance());
    }
};

}  // namespace

FCITX_ADDON_FACTORY_V2(anime, AnimeAddonFactory);
