import argparse
import ctypes
from ctypes import wintypes
from collections import deque
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import threading
import time
import tkinter as tk
from tkinter import messagebox, ttk


GIB = 1024 ** 3
BG, INK, MUTED, ACCENT = "#F5F6F8", "#17232D", "#4B5C68", "#126956"


def confirmed_bytes(state_path):
    with state_path.open("rb") as stream:
        entries = json.load(stream)["upload_state"] or []
    return {
        Path(item["path"]): sum(
            block["range"]["end"] - block["range"].get("begin", 0)
            for block in item["state"]["block_list"]
            if block.get("checksum")
        )
        for item in entries if item.get("state")
    }


def check_remote(remote, files, results):
    env = os.environ.copy()
    env["NO_PROXY"] = ",".join(filter(None, [env.get("NO_PROXY"), ".baidu.com"]))
    for path, size in files.items():
        target = remote.rstrip("/") + "/" + path.name
        try:
            run = subprocess.run(
                ["BaiduPCS-Go", "meta", target], capture_output=True, timeout=15,
                env=env, creationflags=subprocess.CREATE_NO_WINDOW,
            )
            output = (run.stdout + run.stderr).decode("utf-8", errors="replace")
            length = re.search(r"^\s*文件大小\s+(\d+),", output, re.M)
            location = re.search(r"^\s*文件路径\s+(.+?)\s*$", output, re.M)
            if length and location and location[1] == target:
                status = "已上传" if int(length[1]) == size else "网盘大小不符"
            elif "31066" in output:
                status = "尚未入盘"
            else:
                status = "网盘查询失败"
        except (OSError, subprocess.TimeoutExpired):
            status = "网盘查询失败"
        results.put((path, status))


def uploader_handle(pid):
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.QueryFullProcessImageNameW.argtypes = [
        wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD),
    ]
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.OpenProcess(0x100000 | 0x1000, False, pid)
    if handle:
        buffer = ctypes.create_unicode_buffer(32768)
        length = wintypes.DWORD(len(buffer))
        if (kernel.QueryFullProcessImageNameW(handle, 0, buffer, ctypes.byref(length))
                and Path(buffer.value).name.lower() == "baidupcs-go.exe"):
            return kernel, handle
        kernel.CloseHandle(handle)
    return kernel, None


class Monitor:
    def __init__(self, window, args):
        self.window = window
        self.files = {p.resolve(): p.stat().st_size for p in args.files}
        self.remote = args.remote
        self.state_path = args.state_dir / "pcs_uploading.json"
        self.kernel, self.handle = uploader_handle(args.pid)
        self.last_bytes = {}
        self.cloud = {}
        self.results = queue.Queue()
        self.worker = None
        self.next_check = 0
        self.samples = deque()
        self.rows = {}

        window.title("百度网盘上传进度")
        window.configure(bg=BG)
        window.columnconfigure(0, weight=1)
        style = ttk.Style(window)
        style.theme_use("clam")
        style.configure("TProgressbar", troughcolor="#DCE3E5", background=ACCENT,
                        borderwidth=0, lightcolor=ACCENT, darkcolor=ACCENT)
        panel = tk.Frame(window, bg=BG, padx=26, pady=20)
        panel.grid(sticky="nsew")
        panel.columnconfigure(0, weight=1)
        self.percent = self.label(panel, "", 23, bold=True, color=ACCENT)
        self.percent.grid(row=0, column=1, sticky="e")
        self.label(panel, "上传进度", 20, bold=True).grid(row=0, column=0, sticky="w")
        self.total_bar = ttk.Progressbar(panel, maximum=100)
        self.total_bar.grid(row=1, column=0, columnspan=2, sticky="ew", pady=(10, 5))
        self.summary = self.label(panel, "", 11, color=MUTED)
        self.summary.grid(row=2, column=0, columnspan=2, sticky="w")
        for i, path in enumerate(self.files):
            row = 3 + 2 * i
            self.label(panel, path.name, 12).grid(row=row, column=0, sticky="w", pady=(14, 3))
            status = self.label(panel, "", 11, color=MUTED)
            status.grid(row=row, column=1, sticky="e", padx=(16, 0), pady=(14, 3))
            bar = ttk.Progressbar(panel, maximum=self.files[path], length=450)
            bar.grid(row=row+1, column=0, columnspan=2, sticky="ew")
            self.rows[path] = bar, status
        self.footer = self.label(panel, "", 10, color=MUTED)
        self.footer.grid(row=3+2*len(self.files), column=0, columnspan=2, sticky="w", pady=(16, 0))
        window.protocol("WM_DELETE_WINDOW", self.close)
        self.tick()

    @staticmethod
    def label(parent, text, size, bold=False, color=INK):
        return tk.Label(parent, text=text, anchor="w", bg=BG, fg=color,
                        font=("Microsoft YaHei UI", size, "bold" if bold else "normal"))

    def tick(self):
        now = time.monotonic()
        while not self.results.empty():
            path, self_status = self.results.get_nowait()
            self.cloud[path] = self_status
        pending = {p: n for p, n in self.files.items() if self.cloud.get(p) != "已上传"}
        if pending and now >= self.next_check and not (self.worker and self.worker.is_alive()):
            self.worker = threading.Thread(target=check_remote, daemon=True,
                                           args=(self.remote, pending, self.results))
            self.worker.start()
            self.next_check = now + 45
        alive = self.handle and self.kernel.WaitForSingleObject(self.handle, 0) == 258
        read_ok = True
        try:
            snapshot = confirmed_bytes(self.state_path)
        except (OSError, ValueError, KeyError, TypeError):
            snapshot, read_ok = {}, False
        total, sent = sum(self.files.values()), 0
        for path, size in self.files.items():
            remote_status = self.cloud.get(path)
            if path in snapshot:
                self.last_bytes[path] = min(snapshot[path], size)
            count = self.last_bytes.get(path, 0)
            if remote_status == "已上传":
                status, count = remote_status, size
            elif remote_status == "网盘大小不符":
                status = remote_status
            elif not alive:
                status = "上传进程已结束"
            elif not read_ok:
                status = "等待读取进度"
            elif path in snapshot and count < size:
                status = "上传中"
            elif path in self.last_bytes:
                status = "等待网盘确认"
            else:
                status = "排队中"
            sent += count
            bar, label = self.rows[path]
            bar["value"] = count
            label.configure(text=f"{status}    {count / size:.1%}" if size else status)
        fraction = sent / total if total else 0
        self.total_bar["value"] = fraction * 100
        self.percent.configure(text=f"{fraction:.1%}")
        self.samples.append((now, sent))
        while len(self.samples) > 1 and self.samples[1][0] < now - 60:
            self.samples.popleft()
        elapsed = now - self.samples[0][0]
        speed = max(0, sent - self.samples[0][1]) / 1024**2 / elapsed if elapsed >= 8 else None
        rate = f"     近一分钟 {speed:.1f} MiB/s" if speed is not None and pending and alive else ""
        self.summary.configure(text=f"{sent/GIB:.2f} / {total/GIB:.2f} GiB     已上传 {len(self.files)-len(pending)}/{len(self.files)}{rate}")
        footer = "网盘查询失败，稍后自动重试" if "网盘查询失败" in self.cloud.values() else "按已确认的分片统计"
        self.footer.configure(text=f"{footer}     {time.strftime('%H:%M:%S')}")
        self.window.after(2000, self.tick)

    def close(self):
        if self.handle:
            self.kernel.CloseHandle(self.handle)
        self.window.destroy()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--remote", required=True)
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--state-dir", type=Path, default=Path(
        os.environ.get("BAIDUPCS_GO_CONFIG_DIR") or Path(os.environ["APPDATA"]) / "BaiduPCS-Go"))
    parser.add_argument("files", nargs="+", type=Path)
    args = parser.parse_args()
    ctypes.windll.user32.SetProcessDPIAware()
    root = tk.Tk()
    try:
        monitor = Monitor(root, args)
    except OSError as error:
        messagebox.showerror("无法监测上传", str(error), parent=root)
        root.destroy()
    else:
        root.mainloop()
