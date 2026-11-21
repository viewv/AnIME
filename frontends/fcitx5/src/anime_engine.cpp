// fcitx5-anime — AnIME 的 Fcitx5 C++20 薄前端插件（Phase 0 可编译骨架）
//
// 契约见 .agents/contracts/ipc-protocol.md 与 .agents/contracts/core-api.md：
// Task 04 将在此接入异步 Unix socket ($XDG_RUNTIME_DIR/anime/engined.sock) 与候选界面逻辑。

#include <fcitx/addonfactory.h>
#include <fcitx/addoninstance.h>
#include <fcitx/addonmanager.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputmethodengine.h>
#include <fcitx/instance.h>

#include <string>

namespace {

class AnimeEngine final : public fcitx::InputMethodEngineV2 {
public:
    explicit AnimeEngine(fcitx::Instance *instance) : instance_(instance) {}

    void keyEvent(const fcitx::InputMethodEntry &, fcitx::KeyEvent &) override {
        // Phase 0 骨架直接透传按键；Task 04 接入 animed 异步预编辑与选词流程。
    }

    void reset(const fcitx::InputMethodEntry &, fcitx::InputContextEvent &) override {}

    std::string subMode(const fcitx::InputMethodEntry &, fcitx::InputContext &) override {
        return "En";
    }

private:
    [[maybe_unused]] fcitx::Instance *instance_;
};

class AnimeAddonFactory final : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override {
        return new AnimeEngine(manager->instance());
    }
};

}  // namespace

FCITX_ADDON_FACTORY_V2(anime, AnimeAddonFactory);
