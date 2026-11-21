# Contract: Open-Source Data Provenance & Storage Formats (`data-format.md`)

> 状态：**LOCKED**。任何词库、错拼集、语言模型、语料或个人词库读写代码必须严格遵循本契约。

---

## 1. 100% 开源洁净原则与 `data/MANIFEST.yaml`

- **开源协议白名单**：仅允许 **MIT**、**Apache-2.0**、**BSD-2-Clause**、**BSD-3-Clause**、**MPL-2.0**、**ISC**、**CC0-1.0 / Public Domain**、**CC-BY-4.0 / CC-BY-SA-4.0**（CC-BY-SA 仅限独立可审查词表，且须显式标明份额与遵循条件；有争议的模型权重禁止混用未授权闭源数据）。
- **严禁来源**：任何商业/闭源输入法（微信、搜狗、百度、微软、Apple 等）的词库、参数表、权重、导出词频或逆向样本。
- **登记要求**：每一份进入构建管线的公开数据集/模型权重必须在 `data/MANIFEST.yaml` 登记：
  - `id`、`status`（`planned` | `ready`）
  - `source_url`、`version`、`license`、`sha256`（下载/构建脚本校验）
  - `build_script`（指向 `scripts/data_pipeline/` 下可复现脚本）
  - `notes`
- 构建产物默认输出至 `data/raw/` 与 `data/compiled/`（均已 `.gitignore`，不直接向 Git 提交大体积二进制包）。

---

## 2. 运行时平台目录布局

引擎通过统一目录发现层读取静态产物与写入个人学习库，禁止在核心计算逻辑中硬编码 Linux 路径：

| 平台 | 用户数据根目录（可被 `ANIME_DATA_DIR` 覆盖） | 模型/词库资产目录 | 个人学习库 |
|---|---|---|---|
| **Linux** | `$XDG_DATA_HOME/anime/`（回退 `~/.local/share/anime/`） | `$XDG_DATA_HOME/anime/models/`（兼容系统 `/usr/share/anime/`） | `$XDG_DATA_HOME/anime/user.db` |
| **macOS**（预留） | `~/Library/Application Support/AnIME/` | `~/Library/Application Support/AnIME/models/` | `~/Library/Application Support/AnIME/user.db` |

---

## 3. 二进制字典与语言模型格式约定

所有紧凑二进制词典（`ANIMELEX`）与 n-gram 表（`ANIMENGR`）共享统一 **16 字节固定文件头**：

```text
Offset  Size  Type     Meaning
0x00    8     [u8; 8]  Magic: b"ANIMELEX" 或 b"ANIMENGR"
0x08    4     u32 LE   Format version（当前 = 1）
0x0C    4     u32 LE   Feature flags（bit0: case_preserved, bit1: phonetic_index, bit2: compact_quantized）
```

- 文件内所有整数偏移、词频、对数概率统一采用 **Little-Endian** 编码；词条 UTF-8 存储。
- 读取时必须校验 Magic 与 Version；版本不匹配返回可读错误，严禁越界/unchecked 解析。

---

## 4. SQLite 本机个人学习表 Schema（`user.db`）

```sql
CREATE TABLE IF NOT EXISTS user_words (
    word        TEXT PRIMARY KEY,
    freq        INTEGER NOT NULL DEFAULT 1,
    last_used   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS user_bigrams (
    prev_word   TEXT NOT NULL,
    word        TEXT NOT NULL,
    freq        INTEGER NOT NULL DEFAULT 1,
    last_used   INTEGER NOT NULL,
    PRIMARY KEY (prev_word, word)
);
```

- 所有写入须在 `privacy == false` 时执行，推荐开启 `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;` 并采用异步/批量事务写入，避免阻塞按键主路径。
- **不存储**敏感密码框输入；`user.db` 始终仅保留在用户本机目录，不做任何网络同步。
