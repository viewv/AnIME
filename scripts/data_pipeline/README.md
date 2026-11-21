# AnIME Open-Source Data Pipeline

本目录存放用于下载、清洗、校验与编译开源英文词库、错拼集及语料的**可复现构建脚本**。

## 约束

1. 所有外部数据源必须在仓库根目录的 [data/MANIFEST.yaml](../../data/MANIFEST.yaml) 登记（来源 URL、版本、开源协议、SHA256、构建脚本路径）。
2. 原始下载与编译生成的二进制产物输出到 `data/raw/` 与 `data/compiled/`（均在 `.gitignore` 忽略列表中，**禁止**提交进 Git）。
3. 本机词典/n-gram 二进制格式遵循 [.agents/contracts/data-format.md](../../.agents/contracts/data-format.md)。
