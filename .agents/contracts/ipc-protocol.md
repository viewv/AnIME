# Contract: Frontend ↔ Daemon IPC Protocol (`ipc-protocol.md`)

> 状态：**LOCKED**。所有跨进程客户端（`frontends/fcitx5`、CLI、未来 macOS 客户端）与守护进程（`crates/animed`）必须严格遵循本协议。

---

## 1. 传输层约定

- **帧格式**：**NDJSON**（每条消息为单行 UTF-8 JSON，以 `\n` 结尾，单行上限 **64 KiB**；不得包含未转义换行符）。
- **传输端点**：
  - **Linux 默认 Unix Socket**：`$XDG_RUNTIME_DIR/anime/engined.sock`（若环境变量未设置则回退 `/tmp/anime-<uid>/engined.sock`）。父目录权限 `0700`，socket 权限 `0600`。
  - **未来 macOS Unix Socket**：`~/Library/Caches/AnIME/engined.sock`（或直连 C-ABI FFI，复用同一 `protocol` 结构）。
  - **测试 / 无桌面通道**：`animed --stdio` 从标准输入逐行读取请求、向标准输出逐行写出响应，完全使用同一协议。

---

## 2. 时序与抗过期不变量（`seq` 与 `revision`）

- 每次键盘按键或选词状态变更请求由客户端携带单调递增的 `seq`（`u64`）。
- **同步快路径**响应携带 `(seq, revision: 0, final_l3: false|true)`；
- 若后台 L3 神经推理在下一按键到达前完成，服务端追加推送一条同 `seq`、`revision: 1`（或递增）且 `final_l3: true` 的 `state` 帧。
- **客户端强制规则**：维护 `last_sent_seq` 与 `last_applied_revision`；任何 `msg.seq < last_sent_seq` 或同 `seq` 下 `msg.revision <= last_applied_revision` 的响应必须直接丢弃，防止异步结果覆盖最新输入状态。

---

## 3. Request 消息定义（Frontend → `animed`）

| `type` | 必填字段 | 可选字段 | 说明 |
|---|---|---|---|
| `hello` | `client`: string, `protocol_version`: u32 | `session_id`: string | 建连握手，校验协议版本（当前为 `1`） |
| `key` | `seq`: u64, `key`: string | `text`: string, `mods`: u32 | 按键事件：`key` 取 `"char"` / `"space"` / `"backspace"` / `"enter"` / `"escape"` / `"digit"`（配 `text`） |
| `select` | `seq`: u64, `index`: usize | — | 选择第 `index` 项候选（0-based，如按 `1` 对应 `index: 0`） |
| `context` | `before`: string | `after`: string | 上报光标周围文本（surrounding text，建议截断至前 256 字符、后 64 字符） |
| `privacy` | `enabled`: bool | — | 隐私框标记；`true` 时禁止学习入库 |
| `reset` | `seq`: u64 | — | 失焦/切输入上下文时重置会话组合态 |
| `config` | `space_commits`: bool, `smart_spacing`: bool, `max_candidates`: usize, `enable_phonetic`: bool, `enable_neural`: bool | — | 下发会话运行配置 |
| `ping` | — | `id`: u64 | 存活探测 |
| `shutdown` | — | — | 请求干净退出（主要供 `--stdio` / 生命周期管理使用） |

### Request JSON 示例

```json
{"type":"hello","client":"fcitx5-anime","protocol_version":1}
{"type":"context","before":"Thank you for your ","after":""}
{"type":"key","seq":42,"key":"char","text":"q"}
{"type":"select","seq":43,"index":0}
{"type":"ping"}
{"type":"shutdown"}
```

---

## 4. Response 消息定义（`animed` → Frontend）

| `type` | 字段 | 说明 |
|---|---|---|
| `hello_ack` | `protocol_version`: u32, `version`: string, `capabilities`: string[] | 返回服务版本与当前启用能力（如 `"l1-fuzzy"`, `"l2-ngram"`, `"l3-neural"`） |
| `state` | `seq`: u64, `revision`: u32, `preedit`: string, `cursor`: usize, `candidates`: Candidate[], `passthrough`: bool, `final_l3`: bool | 更新预编辑与候选列表；`passthrough: true` 表示键未被消费，前端应透传给应用 |
| `commit` | `seq`: u64, `text`: string, `delete_before`: usize, `remaining_preedit`: string, `candidates`: Candidate[] | 上屏文本；`delete_before` 指示前置回删字符数；部分选词时 `remaining_preedit` 非空 |
| `pong` | `version`: string | `ping` 响应（Phase 0 兼容字段：`{"type":"pong","version":"0.1.0"}`） |
| `error` | `seq`: u64?, `code`: string, `message`: string | 协议错误或内部告警，不应杀死连接 |

### `Candidate` 字段结构

```json
{
  "text": "apple",
  "kind": "TypoCorrection",
  "comment": "typo",
  "score": -1.24,
  "consumed_chars": 5,
  "trailing_space": true
}
```

- `kind` 取值枚举：`Exact` | `Completion` | `TypoCorrection` | `PhoneticCorrection` | `Sentence` | `Continuation` | `Raw`。
- 前端展示快捷键为 1-based（列表索引 `0` 显示为 `1.`，依此类推），选中后发 `{"type":"select","seq":...,"index":0}`。
