# Contract: Core Engine API & Semantics (`core-api.md`)

> 状态：**LOCKED**。所有核心 crate（`protocol` / `lexicon` / `lm-ngram` / `lm-neural` / `engine` / `animed`）与前端必须严格遵守。必须变更本契约时，先暂停任务并在 `Blocked` 栏申请。

---

## 1. 平台无关约束

- `crates/protocol`、`crates/lexicon`、`crates/lm-ngram`、`crates/lm-neural`、`crates/engine` **严禁**直接依赖 Linux、Wayland、X11、Fcitx5、AppKit 或 IMKit 专有类型与系统调用。
- 仅 `crates/animed` 和 `frontends/*` 可处理 OS 路径、Unix socket、事件循环或桌面工具包接口。

---

## 2. 拼音式英文预编辑与交互语义

1. **带空格预编辑（默认行为）**：
   - 配置项 `space_commits: bool` 默认为 **`false`**。
   - 当 `space_commits == false` 且预编辑区正在组合时，按 `Space` 在预编辑末尾追加一个词间空格 `' '`，允许用户连续输入短语或整句（例如 `i wnt to eat an appie`）后由 L2/L3 联合解码整句纠错。
   - 当 `space_commits == true` 且正在组合时，按 `Space` 视同选择第 1 候选上屏。
   - 当预编辑为空时，无论 `space_commits` 为何值，`Space` 一律直接放行给宿主应用（由前端处理或直接透传）。
2. **数字键选词上屏（`1`–`9`）**：
   - 当预编辑非空时，`1`–`9` 选择当前页第 1–9 项候选上屏；当预编辑为空时，数字键**必须原样透传**给应用。
   - **部分选词（Partial Commit）**：当候选条目带有 `consumed_chars < preedit.chars().count()`（例如整串 `i wnt appie` 中用户只选定第一个词对应前缀），只上屏该词并保留剩余未确认预编辑继续组合；若 `consumed_chars == total_chars`（或整句/续写候选），清空预编辑完成本次组合。
3. **原串逃生与取消**：
   - `Enter`：预编辑非空时，**原样上屏当前预编辑文本**并清空预编辑（不做纠错、不自动加额外尾空格），方便代码标识符、缩写与专有名词输入；预编辑为空时透传 `Enter`。
   - `Escape`：清空当前预编辑与候选列表，不上屏任何字符（预编辑为空时透传）。
   - `Backspace`：预编辑非空时删除最后一个字符（若清空则回到 Idle）；预编辑为空且之前刚触发过智能尾空格时，前端/引擎协同执行标点或自动空格回删。
   - `Shift` 单击直通切换：由前端（如 Fcitx5）维护直通模式；若切换到直通时正在组合，先原样上屏现有预编辑。

---

## 3. 候选类型与强制兜底

每条候选携带 `CandidateKind`：
- `Exact`：词典精确匹配（含规范大小写）
- `Completion`：前缀补全（含纠错前缀补全，如 `beutif → beautiful`）
- `TypoCorrection`：QWERTY 邻键 / 漏敲 / 多敲 / 换位纠错（如 `appie → apple`）
- `PhoneticCorrection`：Double Metaphone 等重度读音纠错（如 `numonia → pneumonia`）
- `Sentence`：多词词格解码或神经整句纠错（如 `I want to eat an apple`）
- `Continuation`：基于上下文的下一词/短语续写（UI 建议前缀展示 `✦`）
- `Raw`：用户键入的原样字母串（**规则：只要 `preedit` 非空，候选列表中必须至少包含原始输入的兜底候选或位于末尾的 `Raw` 项，保证用户永远有路可退**）

---

## 4. 智能空格、标点回删、大小写与缩写恢复

- **智能尾空格（`smart_spacing: true` 默认开启）**：
  - 选词或整句上屏时，默认在末尾附带 1 个半角空格 `' '`（`Enter` 原样上屏与 `Raw` 项不强加尾空格，或依明确配置）。
  - **标点自动吞空格**：若紧跟在一次由 AnIME 插入的智能尾空格之后立即输入半角终止标点（`.` `,` `?` `!` `;` `:` `)` `]` `"`），需先回删刚才附带的 1 个尾空格再输出标点（`receive ` + `.` → `receive.`），确保英文排版自然。
- **大小写与缩写恢复**：
  - 独立单词 `i` 自动纠正为 `I`；常见漏撇号缩写（`im → I'm`、`ive → I've`、`dont → don't`、`cant → can't`、`wont → won't`、`isnt → isn't`、`didnt → didn't`）进入高优候选。
  - 光标前为句首或句末标点+空格（`. `、`! `、`? `、换行）时，自动提示首字母大写候选。

---

## 5. 隐私保护与延迟预算

- **隐私模式（`privacy: true`）**：当宿主应用输入焦点为密码框/隐私框时，前端立即发 `privacy(true)`；引擎**立即停止记录 `user_words` / `user_bigrams`**，且可选停用历史个性化提升。
- **延迟预算**：
  - **L1 + L2 同步快速路径**（按**单次按键** `handle_key` / `select` 调用计时，不含 IPC 与前端渲染）：`p50 <= 3 ms`，`p99 <= 10 ms`，预编辑 `<= 80` 字符时单键最大值 `<= 16 ms`（绝不因磁盘或重算阻塞按键）。
    - Why：按键到屏幕需经 前端 → IPC → 引擎 → IPC → 合成器，60 Hz 一帧约 16.7 ms；引擎稳定在一帧 1/3 以内即不可感知。上限仍需收紧，因为同步路径串行处理按键（连打键间隔可低至 30–50 ms，超时即排队拖键）、同步耗时直接挤占其后启动的 L3 预算，且低功耗/降频 CPU 会慢 2–3 倍。
  - **L3 神经异步精排/续写路径**：单次推理预算 `<= 30 ms`，通过单调递增 `seq` 与同一 `seq` 下的 `revision` 异步推回最新候选；旧 `seq` 的任何晚到结果必须丢弃。
