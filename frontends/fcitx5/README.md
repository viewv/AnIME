# AnIME Fcitx5 前端插件 (`frontends/fcitx5`)

面向 **Linux Wayland / X11 + Fcitx5** 的 C++20 薄前端插件，负责按键拦截/直通切换、Preedit 展示、CandidateList 渲染、Surrounding Text 上下文采集、隐私框（Password/Sensitive）隔离，并通过非阻塞 Unix Domain Socket NDJSON IPC 与 `animed` 守护进程通信。

---

## 1. 本地编译与单元测试

在仓库根目录执行：

```bash
# 编译 libanime.so 与独立 C++ 单元测试
cmake -S frontends/fcitx5 -B frontends/fcitx5/build
cmake --build frontends/fcitx5/build -j$(nproc)

# 运行 C++ 单元测试
ctest --test-dir frontends/fcitx5/build --output-on-failure

# 编译 animed 守护进程（Release）
cargo build --release -p animed
```

---

## 2. 安装到系统与启用方式（需用户自行执行）

> **注意**：安装插件到 `/usr` 需要管理员权限，请由用户在终端中自行核对并执行：

```bash
# 安装 libanime.so 与 anime.conf（addon / inputmethod 配置）
sudo cmake --install frontends/fcitx5/build --prefix /usr

# （可选）安装 animed 守护进程到 /usr/local/bin
sudo install -Dm755 target/release/animed /usr/local/bin/animed
```

若不安装系统包，也可先直接用 `ANIME_DATA_DIR` 或在仓库根目录运行构建好的守护进程测试。`animed` 默认自动探测：
- Socket：`$XDG_RUNTIME_DIR/anime/engined.sock`（回退 `/tmp/anime-<uid>/engined.sock`）
- 词库与 N-gram 资产：`$ANIME_DATA_DIR` / `$XDG_DATA_HOME/anime` / `/usr/share/anime` / `data/compiled/`

### Systemd User Service 示例（`~/.config/systemd/user/animed.service`）

```ini
[Unit]
Description=AnIME English Input Method Daemon
After=graphical-session.target

[Service]
ExecStart=/usr/local/bin/animed --data-dir %h/.local/share/anime
Restart=on-failure
RestartSec=1s

[Install]
WantedBy=default.target
```

若本地尚未将 `data/compiled/lexicon.animelex.bin` 与 `data/compiled/ngram.animengr.bin` 复制到 `~/.local/share/anime/`，可先复制过去（或将 `WorkingDirectory=/path/to/AnIME` 配到 service 中）：

```bash
mkdir -p ~/.local/share/anime
cp -v data/compiled/lexicon.animelex.bin data/compiled/ngram.animengr.bin ~/.local/share/anime/
systemctl --user daemon-reload
systemctl --user enable --now animed.service
```

### 在 Fcitx5 中启用与配置 AnIME

1. 若当前桌面已在运行 Fcitx5（如 KDE Wayland 虚拟键盘方案通过 KWin 自动拉起 `fcitx5`），请在桌面设置 **系统设置 → 虚拟键盘 / 输入法** 中重载或在 Fcitx5 托盘图标右键 **重新启动**，**不要在 KDE Wayland 会话里用命令行强行手起第二个 `fcitx5` 进程**。
2. 打开 **Fcitx5 配置**（`fcitx5-configtool`），在“当前输入法”中添加 **AnIME**（中文界面显示 **AnIME 英文**）。
3. 点击 **AnIME** 右侧配置图标（或“附加组件 → AnIME”），即可直接在图形界面中实时修改以下选项（保存后自动同步给 `animed` 热生效，配置持久化于 `~/.config/fcitx5/conf/anime.conf`）：
   - **`SmartSpacing`（默认开启）**：选词或整句上屏后自动追加 1 个半角空格（并在紧跟 `.` `,` `?` `!` `;` `:` `)` `]` `"` 时自动吞回删该空格）；**不习惯自动空格可在此直接关闭**。
   - **`SpaceCommits`（默认关闭）**：按 `Space` 时是将空格留在预编辑里输入整句（关），还是直接上屏第 1 候选（开）。
   - **`CandidateLayout`（默认 `Auto`）**：`Auto` 会在多词整句或候选较长时自动切为竖排并按页展示，单词保持 Fcitx5 主题默认/横排；也可强制设成 `Vertical` 或 `Horizontal`。
   - **`MaxCandidates`（默认 `9`）**：最大候选数量（`3`–`9`）。
   - **`LongCandidatePageSize`（默认 `5`）**：长句/多词模式每页显示候选数（`3`–`9`），超出部分可用 `PageDown` / `PageUp` 翻页。
   - **`ShowCandidateComments`（默认开启）**：显示 `typo` / `phonetic` / `raw` / `next` 等提示标签（已自动省略整句候选重复的 `sentence` 标签）。
   - **`EnablePhonetic` / `EnableNeural`**：读音纠错与神经模型重排/续写开关。

---

## 3. 按键操作说明

| 按键 | 空闲态（无预编辑） | 组合态（预编辑非空） |
|---|---|---|
| `a`–`z`, `A`–`Z` | 启动预编辑并发送 `animed` | 追加到当前词/句预编辑 |
| `'` / `-` | 原样透传 | 作为词内连字/缩写字符加入预编辑 |
| `Space` | 原样透传 | 默认向预编辑追加空格，支持多词/整句组合纠错（若启用 `SpaceCommits` 则上屏首项） |
| `PageDown` / `PageUp` | 原样透传 | 多页候选时翻页（查看后半部分长句/兜底候选） |
| `Down` / `Up` | 原样透传 | 移动当前页候选高亮光标 |
| `1`–`9` | 原样透传（含数字输入） | 选择当前页第 `1`–`9` 候选上屏（支持前缀部分选词） |
| `Enter` | 原样透传 | **原样上屏当前预编辑文本**，不做纠错且不加尾空格 |
| `Escape` | 原样透传 | 清空当前预编辑，不上屏 |
| `Backspace` | 原样透传 | 删除最后一个预编辑字符 |
| `.` `,` `?` `!` `;` `:` `)` `]` `"` | 若开启 `SmartSpacing` 且紧跟上一条智能尾空格，自动回删 1 个空格再上屏标点；否则透传 | 原样透传 |
| **单击 `Shift` 键** | 切换直通模式（状态显示 `En` / `Raw`） | 若当前正在组合，先原样上屏现有预编辑，再切为 `Raw` 直通模式 |
