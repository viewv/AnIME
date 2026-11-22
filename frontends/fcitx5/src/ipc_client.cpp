#include "ipc_client.h"

#include <poll.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

#include <cerrno>
#include <cstring>

namespace anime::ipc {
namespace {

constexpr size_t kMaxWriteBufferBytes = 256 * 1024;
constexpr size_t kRecvChunkBytes = 8192;

std::string parentDirectory(const std::string &path) {
    const size_t slash = path.rfind('/');
    if (slash == std::string::npos) {
        return ".";
    }
    if (slash == 0) {
        return "/";
    }
    return path.substr(0, slash);
}

}  // namespace

std::string resolveSocketPath() {
    if (const char *override_path = std::getenv("ANIME_SOCKET_PATH")) {
        if (*override_path != '\0') {
            return std::string(override_path);
        }
    }
    if (const char *xdg = std::getenv("XDG_RUNTIME_DIR")) {
        if (*xdg != '\0') {
            std::string path(xdg);
            if (path.back() != '/') {
                path.push_back('/');
            }
            path.append("anime/engined.sock");
            return path;
        }
    }
    return "/tmp/anime-" + std::to_string(static_cast<unsigned long>(::geteuid())) +
           "/engined.sock";
}

bool validateSocketPermissions(const std::string &socket_path, std::string *error_out) {
    auto set_err = [&](std::string msg) {
        if (error_out) {
            *error_out = std::move(msg);
        }
        return false;
    };

    const std::string parent = parentDirectory(socket_path);
    struct stat dir_st {};
    if (::lstat(parent.c_str(), &dir_st) != 0) {
        return set_err("cannot stat socket parent dir '" + parent + "': " + std::strerror(errno));
    }
    if (!S_ISDIR(dir_st.st_mode)) {
        return set_err("socket parent '" + parent + "' is not a directory");
    }
    const uid_t uid = ::geteuid();
    if (dir_st.st_uid != uid) {
        return set_err("socket parent '" + parent + "' is not owned by current user");
    }
    if ((dir_st.st_mode & 0077U) != 0) {
        return set_err("socket parent '" + parent + "' has insecure permissions (expected 0700)");
    }

    struct stat sock_st {};
    if (::lstat(socket_path.c_str(), &sock_st) != 0) {
        return set_err("cannot stat socket '" + socket_path + "': " + std::strerror(errno));
    }
    if (!S_ISSOCK(sock_st.st_mode)) {
        return set_err("path '" + socket_path + "' is not a unix socket");
    }
    if (sock_st.st_uid != uid) {
        return set_err("socket '" + socket_path + "' is not owned by current user");
    }
    if ((sock_st.st_mode & 0077U) != 0) {
        return set_err("socket '" + socket_path + "' has insecure permissions (expected 0600)");
    }
    return true;
}

IpcClient::~IpcClient() {
    disconnect();
}

bool IpcClient::connect(const std::string &socket_path, std::string *error_out) {
    disconnect();

    auto set_err = [&](std::string msg) {
        if (error_out) {
            *error_out = std::move(msg);
        }
        return false;
    };

    sockaddr_un addr{};
    if (socket_path.empty() || socket_path.size() >= sizeof(addr.sun_path)) {
        return set_err("invalid unix socket path length");
    }

    if (!validateSocketPermissions(socket_path, error_out)) {
        return false;
    }

    const int fd = ::socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    if (fd < 0) {
        return set_err(std::string("socket() failed: ") + std::strerror(errno));
    }

    addr.sun_family = AF_UNIX;
    std::memcpy(addr.sun_path, socket_path.data(), socket_path.size());
    addr.sun_path[socket_path.size()] = '\0';

    const socklen_t addr_len =
        static_cast<socklen_t>(offsetof(sockaddr_un, sun_path) + socket_path.size() + 1);
    if (::connect(fd, reinterpret_cast<const sockaddr *>(&addr), addr_len) == 0) {
        fd_ = fd;
        state_ = State::Connected;
        return true;
    }

    if (errno == EINPROGRESS) {
        fd_ = fd;
        state_ = State::Connecting;
        return true;
    }

    const int saved_errno = errno;
    ::close(fd);
    return set_err(std::string("connect() failed: ") + std::strerror(saved_errno));
}

void IpcClient::disconnect() {
    if (fd_ >= 0) {
        ::close(fd_);
        fd_ = -1;
    }
    state_ = State::Disconnected;
    write_buf_.clear();
    read_buf_.clear();
    discarding_oversized_line_ = false;
}

bool IpcClient::enqueueLine(std::string_view json_line) {
    if (state_ == State::Disconnected || fd_ < 0) {
        return false;
    }
    if (json_line.size() > kMaxIpcLineBytes || json_line.find('\n') != std::string_view::npos ||
        json_line.find('\r') != std::string_view::npos) {
        return false;
    }
    if (write_buf_.size() + json_line.size() + 1 > kMaxWriteBufferBytes) {
        // 对端长时间无响应导致积压超限时主动断开并走 fail-open 重连，防止阻塞按键链路。
        return false;
    }

    write_buf_.append(json_line);
    write_buf_.push_back('\n');

    if (state_ == State::Connected) {
        return flushWrites();
    }
    return true;
}

bool IpcClient::flushWrites() {
    while (!write_buf_.empty()) {
        const ssize_t n =
            ::send(fd_, write_buf_.data(), write_buf_.size(), MSG_DONTWAIT | MSG_NOSIGNAL);
        if (n > 0) {
            write_buf_.erase(0, static_cast<size_t>(n));
            continue;
        }
        if (n < 0 && errno == EINTR) {
            continue;
        }
        if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            return true;
        }
        return false;
    }
    return true;
}

bool IpcClient::onWritable() {
    if (state_ == State::Disconnected || fd_ < 0) {
        return false;
    }
    if (state_ == State::Connecting) {
        int so_error = 0;
        socklen_t len = sizeof(so_error);
        if (::getsockopt(fd_, SOL_SOCKET, SO_ERROR, &so_error, &len) != 0 || so_error != 0) {
            return false;
        }
        state_ = State::Connected;
    }
    return flushWrites();
}

void IpcClient::handleLine(std::string_view line,
                           const std::function<void(const IpcResponse &)> &on_message) {
    if (line.ends_with('\r')) {
        line.remove_suffix(1);
    }
    if (line.size() > kMaxIpcLineBytes) {
        return;
    }
    bool all_ws = true;
    for (char c : line) {
        if (c != ' ' && c != '\t') {
            all_ws = false;
            break;
        }
    }
    if (all_ws) {
        return;
    }
    if (auto resp = decodeResponseLine(line)) {
        if (on_message) {
            on_message(*resp);
        }
    }
}

bool IpcClient::onReadable(const std::function<void(const IpcResponse &)> &on_message) {
    if (state_ == State::Disconnected || fd_ < 0) {
        return false;
    }
    char chunk[kRecvChunkBytes];
    while (state_ != State::Disconnected && fd_ >= 0) {
        const ssize_t n = ::recv(fd_, chunk, sizeof(chunk), MSG_DONTWAIT);
        if (n == 0) {
            return false;
        }
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                return true;
            }
            return false;
        }

        std::string_view view(chunk, static_cast<size_t>(n));
        while (!view.empty()) {
            const size_t nl = view.find('\n');
            if (discarding_oversized_line_) {
                if (nl == std::string_view::npos) {
                    view = {};
                } else {
                    discarding_oversized_line_ = false;
                    read_buf_.clear();
                    view.remove_prefix(nl + 1);
                }
                continue;
            }

            if (nl == std::string_view::npos) {
                if (read_buf_.size() + view.size() > kMaxIpcLineBytes + 1) {
                    read_buf_.clear();
                    discarding_oversized_line_ = true;
                    view = {};
                } else {
                    read_buf_.append(view);
                    view = {};
                }
                continue;
            }

            const std::string_view segment = view.substr(0, nl);
            if (read_buf_.size() + segment.size() > kMaxIpcLineBytes + 1) {
                read_buf_.clear();
                view.remove_prefix(nl + 1);
                continue;
            }

            if (read_buf_.empty()) {
                handleLine(segment, on_message);
            } else {
                read_buf_.append(segment);
                std::string complete;
                complete.swap(read_buf_);
                handleLine(complete, on_message);
            }
            view.remove_prefix(nl + 1);
            if (state_ == State::Disconnected || fd_ < 0) {
                return true;
            }
        }
    }
    return true;
}

bool IpcClient::pollOnce(int timeout_ms,
                         const std::function<void(const IpcResponse &)> &on_message) {
    if (state_ == State::Disconnected || fd_ < 0) {
        return false;
    }
    short events = POLLIN | POLLERR | POLLHUP;
    if (state_ == State::Connecting || hasPendingWrite()) {
        events |= POLLOUT;
    }
    pollfd pfd{fd_, events, 0};
    const int rc = ::poll(&pfd, 1, timeout_ms);
    if (rc < 0) {
        return errno == EINTR;
    }
    if (rc == 0) {
        return true;
    }
    if ((pfd.revents & POLLOUT) != 0) {
        if (!onWritable()) {
            return false;
        }
    }
    if ((pfd.revents & POLLIN) != 0) {
        if (!onReadable(on_message)) {
            return false;
        }
    }
    if ((pfd.revents & (POLLERR | POLLHUP | POLLNVAL)) != 0 &&
        (pfd.revents & POLLIN) == 0) {
        return false;
    }
    return true;
}

}  // namespace anime::ipc
