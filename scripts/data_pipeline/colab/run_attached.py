"""Run train_l3_correction.py as a child process while keeping the Colab kernel busy.

Why: a Colab runtime with an idle kernel and no attached frontend is reclaimed after
~30-40 minutes even while a background `nohup` job is running (observed with
`colab ssh`). Executing this file through `colab exec -f --timeout 600` keeps a kernel
cell running and the CLI websocket attached for the whole job. A heartbeat line is
printed every HEARTBEAT_S seconds because `colab exec` aborts locally after 30 s of
kernel silence by default.

Arguments come from the ANIME_TRAIN_ARGS environment variable (shell-split) or the
defaults below.
"""
import os
import shlex
import subprocess
import sys
import time

HEARTBEAT_S = 20

ROOT = "/content/AnIME_l3"
args = shlex.split(os.environ.get("ANIME_TRAIN_ARGS", "--smoke --out-dir /content/AnIME_l3/smoke_out"))
log_path = os.environ.get("ANIME_TRAIN_LOG", f"{ROOT}/train.log")
# 训练脚本按 transformers 4.x 验证；Colab 默认已是 5.x
subprocess.run([sys.executable, "-m", "pip", "install", "-q", "transformers==4.57.6"], check=True)
cmd = [sys.executable, f"{ROOT}/train_l3_correction.py", "--data-dir", f"{ROOT}/data", *args]
print("launch:", " ".join(cmd), flush=True)
with open(log_path, "w") as log:
    proc = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, cwd=ROOT)
    last = ""
    while proc.poll() is None:
        time.sleep(HEARTBEAT_S)
        with open(log_path, "rb") as f:
            f.seek(max(0, os.path.getsize(log_path) - 4000))
            tail = f.read().decode("utf-8", "replace").replace("\r", "\n").strip().splitlines()
        line = next((l for l in reversed(tail) if l.strip()), "")
        print(time.strftime("%H:%M:%S"), line[-200:] if line != last else ".", flush=True)
        last = line
print("exit code", proc.returncode, flush=True)
with open(log_path, encoding="utf-8", errors="replace") as f:
    print("".join(l for l in f if l.startswith("[l3-train]") or "Error" in l)[-4000:], flush=True)
