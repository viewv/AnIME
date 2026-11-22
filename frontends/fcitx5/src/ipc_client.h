#ifndef ANIME_FCITX5_IPC_CLIENT_H
#define ANIME_FCITX5_IPC_CLIENT_H

#include "ipc_protocol.h"

#include <functional>
#include <string>
#include <string_view>

namespace anime::ipc {

// 解析 animed Unix Socket 路径：
// 1. ANIME_SOCKET_PATH 环境变量（测试与自定义覆盖）；
// 2. $XDG_RUNTIME_DIR/anime/engined.sock；
// 3. 回退 /tmp/anime-<uid>/engined.sock。
std::string resolveSocketPath();

// 校验父目录 (0700, 当前 UID) 与 socket 文件 (0600, 当前 UID)，防止连接不受信任的恶意套接字。
bool validateSocketPermissions(const std::string &socket_path, std::string *error_out = nullptr);

// 非阻塞 Unix Domain Socket NDJSON 客户端（无 Fcitx5 依赖，可直接用于 Fcitx5 EventLoop 或独立测试）。
class IpcClient {
public:
    enum class State {
        Disconnected,
        Connecting,
        Connected,
    };

    IpcClient() = default;
    ~IpcClient();

    IpcClient(const IpcClient &) = delete;
    IpcClient &operator=(const IpcClient &) = delete;

    bool connect(const std::string &socket_path, std::string *error_out = nullptr);
    void disconnect();

    State state() const { return state_; }
    bool isConnected() const { return state_ == State::Connected; }
    bool isConnecting() const { return state_ == State::Connecting; }
    bool hasPendingWrite() const { return !write_buf_.empty(); }
    int fd() const { return fd_; }

    bool enqueueLine(std::string_view json_line);
    bool onWritable();
    bool onReadable(const std::function<void(const IpcResponse &)> &on_message);

    // 仅供独立单元测试/同步工具使用的 poll(2) 驱动辅助方法；Fcitx5 插件内由 EventLoop 驱动。
    bool pollOnce(int timeout_ms, const std::function<void(const IpcResponse &)> &on_message);

private:
    bool flushWrites();
    void handleLine(std::string_view line,
                    const std::function<void(const IpcResponse &)> &on_message);

    int fd_ = -1;
    State state_ = State::Disconnected;
    std::string write_buf_;
    std::string read_buf_;
    bool discarding_oversized_line_ = false;
};

}  // namespace anime::ipc

#endif  // ANIME_FCITX5_IPC_CLIENT_H
