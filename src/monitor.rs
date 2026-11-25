//! 自动发现运行中的 BaiduPCS-Go 上传／下载任务并计算进度；提供暂停、继续、停止、续传、移除。
//!
//! - 上传进度：配置目录下 `pcs_uploading.json` 中已确认（有 checksum）的分片。
//! - 下载进度：目标文件旁 `<文件>.BaiduPCS-Go-downloading` 断点文件（protobuf）。
//! - 上传完成：文件离开状态文件后用同一个 BaiduPCS-Go 运行 `meta` 核对网盘大小。

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::platform;

const SIDECAR: &str = ".BaiduPCS-Go-downloading";
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const MIB: f64 = 1024.0 * 1024.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Down,
    Up,
    Done,
}

/// 卡片操作模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 可暂停、停止
    Running,
    /// 可继续、停止
    Paused,
    /// 已中断：可续传、移除
    Stopped,
    /// 网盘大小不符：只能移除
    Failed,
    None,
}

#[derive(Debug)]
pub struct Task {
    pub kind: Kind,
    pub path: PathBuf,
    key: String,
    pub total: u64,
    pub done: u64,
    pub rate: f64,
    pub status: &'static str,
    remote: Option<String>,
    cloud: Option<&'static str>,
    active: bool,
    alive: bool,
    started: bool,
    dead_checked: bool,
    pub paused: bool,
    pids: BTreeSet<u32>,
    samples: VecDeque<(f64, u64)>,
    /// 最近一次看到的 BaiduPCS-Go 命令行，用于中断后续传
    command: Option<Vec<String>>,
    restarted_at: Option<f64>,
}

impl Task {
    fn new(kind: Kind, path: PathBuf, key: String) -> Self {
        Task {
            kind,
            path,
            key,
            total: 0,
            done: 0,
            rate: 0.0,
            status: "",
            remote: None,
            cloud: None,
            active: false,
            alive: false,
            started: false,
            dead_checked: false,
            paused: false,
            pids: BTreeSet::new(),
            samples: VecDeque::new(),
            command: None,
            restarted_at: None,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    pub fn tab(&self) -> Tab {
        match (self.status, self.kind) {
            ("已完成", _) => Tab::Done,
            (_, Kind::Up) => Tab::Up,
            (_, Kind::Down) => Tab::Down,
        }
    }
    pub fn running(&self) -> bool {
        matches!(self.status, "上传中" | "下载中")
    }
    fn finished(&self) -> bool {
        self.cloud == Some("已完成") || self.status == "已完成"
    }
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64) as f32
        }
    }
    pub fn mode(&self) -> Mode {
        match self.status {
            "已暂停" => Mode::Paused,
            "已中断" => Mode::Stopped,
            "网盘大小不符" => Mode::Failed,
            "上传中" | "下载中" | "准备中" | "排队中" | "等待网盘确认" if !self.pids.is_empty() => {
                Mode::Running
            }
            _ => Mode::None,
        }
    }
    pub fn can_restart(&self) -> bool {
        self.command.is_some()
    }
    pub fn detail(&self) -> String {
        let mut parts = vec![
            self.status.to_string(),
            format!(
                "{:.2} / {:.2} GB",
                self.done as f64 / GIB,
                self.total as f64 / GIB
            ),
        ];
        if self.rate > 0.0 {
            parts.push(format!("{:.1} MB/s", self.rate / MIB));
            parts.push(format!(
                "剩余 {}",
                duration((self.total.saturating_sub(self.done)) as f64 / self.rate)
            ));
        }
        parts.join("   ·   ")
    }
}

pub fn duration(seconds: f64) -> String {
    let s = seconds as u64;
    if s >= 3600 {
        format!("{}小时{}分", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{}分{}秒", s / 60, s % 60)
    } else {
        format!("{s}秒")
    }
}

// ---------- BaiduPCS-Go 文件格式 ----------

fn varint(data: &[u8], i: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let byte = *data.get(*i)?;
        *i += 1;
        value |= u64::from(byte & 0x7F).checked_shl(shift)?;
        if byte < 0x80 {
            return Some(value);
        }
        shift += 7;
    }
}

/// 最小 protobuf 读取：返回 (字段号, varint 值, 长度分隔字节)。
fn proto_fields(data: &[u8]) -> Option<Vec<(u64, u64, &[u8])>> {
    let mut fields = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let tag = varint(data, &mut i)?;
        match tag & 7 {
            0 => fields.push((tag >> 3, varint(data, &mut i)?, &data[0..0])),
            2 => {
                let size = varint(data, &mut i)? as usize;
                let bytes = data.get(i..i.checked_add(size)?)?;
                fields.push((tag >> 3, 0, bytes));
                i += size;
            }
            1 => i += 8,
            5 => i += 4,
            _ => return None,
        }
    }
    Some(fields)
}

/// 下载断点文件 → (总大小, 已下载)。字段：2=总大小 3=下一个未分配偏移 5=未完成区间{1=begin, 2=end}。
pub fn download_progress(data: &[u8]) -> Option<(u64, u64)> {
    let (mut total, mut assigned, mut pending) = (0u64, 0u64, 0u64);
    for (field, value, bytes) in proto_fields(data)? {
        match field {
            2 => total = value,
            3 => assigned = assigned.max(value),
            5 => {
                let (mut begin, mut end) = (0u64, 0u64);
                for (f, v, _) in proto_fields(bytes)? {
                    match f {
                        1 => begin = v,
                        2 => end = v,
                        _ => {}
                    }
                }
                assigned = assigned.max(end);
                pending += end.saturating_sub(begin);
            }
            _ => {}
        }
    }
    Some((total, assigned.saturating_sub(pending).min(total)))
}

struct UploadEntry {
    path: PathBuf,
    total: u64,
    done: u64,
    started: bool,
}

fn read_uploads(state_dir: &Path) -> Option<HashMap<String, UploadEntry>> {
    let json: Value =
        serde_json::from_slice(&std::fs::read(state_dir.join("pcs_uploading.json")).ok()?).ok()?;
    let mut result = HashMap::new();
    for item in json
        .get("upload_state")?
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let blocks = item.pointer("/state/block_list").and_then(Value::as_array);
        let done = blocks
            .into_iter()
            .flatten()
            .filter(|b| {
                b.get("checksum")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
            })
            .map(|b| {
                let begin = b
                    .pointer("/range/begin")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                b.pointer("/range/end")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .saturating_sub(begin)
            })
            .sum();
        let Some(path) = item.get("path").and_then(Value::as_str).map(absolute) else {
            continue;
        };
        result.insert(
            key_of(&path),
            UploadEntry {
                total: item.get("length").and_then(Value::as_u64).unwrap_or(0),
                done,
                started: blocks.is_some_and(|b| !b.is_empty()),
                path,
            },
        );
    }
    Some(result)
}

/// BaiduPCS-Go 的配置目录（pcsconfig.GetConfigDir）：环境变量 → exe 旁已有 pcs_config.json → 系统默认。
pub fn state_dir(exe: &Path) -> PathBuf {
    let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    if let Some(dir) = std::env::var_os("BAIDUPCS_GO_CONFIG_DIR") {
        let dir = PathBuf::from(dir);
        return if dir.is_absolute() {
            dir
        } else {
            exe_dir.join(dir)
        };
    }
    if exe_dir.join("pcs_config.json").exists() {
        return exe_dir;
    }
    let home = if cfg!(windows) {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("BaiduPCS-Go"))
    } else {
        std::env::var_os("HOME").map(|d| PathBuf::from(d).join(".config").join("BaiduPCS-Go"))
    };
    home.unwrap_or(exe_dir)
}

fn download_dir(argv: &[String], state_dir: &Path, exe: &Path) -> PathBuf {
    for (i, arg) in argv.iter().enumerate() {
        if matches!(arg.as_str(), "--saveto" | "-saveto")
            && let Some(dir) = argv.get(i + 1)
        {
            return PathBuf::from(dir);
        }
        if let Some(dir) = arg
            .strip_prefix("--saveto=")
            .or_else(|| arg.strip_prefix("-saveto="))
        {
            return PathBuf::from(dir);
        }
    }
    let saved = std::fs::read(state_dir.join("pcs_config.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|c| {
            c.get("savedir")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        });
    // ponytail: download --save（存到进程当前目录）无法从外部取得目录，未覆盖
    saved.unwrap_or_else(|| exe.parent().unwrap_or(Path::new(".")).join("Downloads"))
}

fn absolute(path: &str) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path))
}

fn key_of(path: &Path) -> String {
    let text = path.to_string_lossy();
    // Windows 文件系统不区分大小写
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.into_owned()
    }
}

fn find_sidecars(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => find_sidecars(&path, out),
            Ok(_) if path.to_string_lossy().ends_with(SIDECAR) => out.push(path),
            _ => {}
        }
    }
}

/// 解析 `BaiduPCS-Go meta` 输出：(文件大小, 文件路径)。
pub fn parse_meta(output: &str) -> (Option<u64>, Option<String>) {
    let (mut size, mut path) = (None, None);
    for line in output.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("文件大小") {
            size = rest
                .trim_start()
                .split(',')
                .next()
                .and_then(|n| n.trim().parse().ok());
        } else if let Some(rest) = line.strip_prefix("文件路径") {
            path = Some(rest.trim().to_string());
        }
    }
    (size, path)
}

fn no_proxy() -> String {
    let current = std::env::var("NO_PROXY").unwrap_or_default();
    // 网盘流量不走代理：经本地代理上传大文件会停滞
    if current.is_empty() {
        "baidu.com,baidupcs.com".into()
    } else {
        format!("{current},baidu.com,baidupcs.com")
    }
}

fn background(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command.stdin(Stdio::null())
}

fn check_remote(exe: &str, target: &str, size: u64) -> &'static str {
    let child = background(Command::new(exe).args(["meta", target]))
        .env("NO_PROXY", no_proxy())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return "网盘查询失败";
    };
    let mut pipes = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    ];
    let readers: Vec<_> = pipes
        .iter_mut()
        .filter_map(Option::take)
        .map(|mut pipe| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = pipe.read_to_end(&mut buf);
                buf
            })
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(15);
    while matches!(child.try_wait(), Ok(None)) {
        if Instant::now() > deadline {
            let _ = child.kill();
            return "网盘查询失败";
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let output: Vec<u8> = readers
        .into_iter()
        .flat_map(|r| r.join().unwrap_or_default())
        .collect();
    let output = String::from_utf8_lossy(&output);
    match parse_meta(&output) {
        (Some(len), Some(path)) if path == target => {
            if len == size {
                "已完成"
            } else {
                "网盘大小不符"
            }
        }
        _ if output.contains("31066") => "尚未入盘",
        _ => "网盘查询失败",
    }
}

// ---------- 监视器 ----------

pub struct Monitor {
    pub tasks: Vec<Task>,
    pub up_rate: f64,
    pub down_rate: f64,
    pub cloud_failed: bool,
    pub tab: Tab,
    system: System,
    uploads: HashMap<PathBuf, HashMap<String, UploadEntry>>,
    results: Arc<Mutex<Vec<(String, &'static str)>>>,
    checking: Option<std::thread::JoinHandle<()>>,
    next_check: f64,
    clock: Instant,
    /// Windows 上记录本程序暂停的进程 (pid, 启动时间)，用于识别暂停状态（含监视器重启后）
    #[cfg(windows)]
    paused: BTreeSet<(u32, u64)>,
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor {
            tasks: Vec::new(),
            up_rate: 0.0,
            down_rate: 0.0,
            cloud_failed: false,
            tab: Tab::Down, // 面板默认选中“下载中”
            system: System::new().expect("sysinfo 不支持当前系统"),
            uploads: HashMap::new(),
            results: Arc::default(),
            checking: None,
            next_check: 0.0,
            clock: Instant::now(),
            #[cfg(windows)]
            paused: crate::store::load("paused.json").unwrap_or_default(),
        }
    }
}

impl Monitor {
    fn task(&mut self, kind: Kind, path: PathBuf) -> &mut Task {
        let key = format!("{kind:?}|{}", key_of(&path));
        match self.tasks.iter().position(|t| t.key == key) {
            Some(i) => &mut self.tasks[i],
            None => {
                self.tasks.push(Task::new(kind, path, key));
                self.tasks.last_mut().unwrap()
            }
        }
    }

    pub fn poll(&mut self) {
        let now = self.clock.elapsed().as_secs_f64();
        for t in &mut self.tasks {
            t.active = false;
            t.alive = false;
            t.pids.clear();
        }
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_exe(UpdateKind::OnlyIfNotSet),
        );
        let mut found = Vec::new();
        for (pid, process) in self.system.processes() {
            if !process
                .name()
                .to_string_lossy()
                .to_lowercase()
                .starts_with("baidupcs-go")
            {
                continue;
            }
            let argv: Vec<String> = process
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            if argv.len() < 2 {
                continue;
            }
            let exe = process
                .exe()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(&argv[0]));
            #[cfg(windows)]
            let suspended = self.paused.contains(&(pid.as_u32(), process.start_time()));
            #[cfg(not(windows))]
            let suspended = process.status() == sysinfo::ProcessStatus::Stop;
            found.push((pid.as_u32(), process.start_time(), exe, argv, suspended));
        }
        #[cfg(windows)]
        {
            // 已退出的进程从暂停记录中移除
            let before = self.paused.len();
            self.paused
                .retain(|p| found.iter().any(|f| (f.0, f.1) == *p));
            if self.paused.len() != before {
                crate::store::save("paused.json", &self.paused);
            }
        }

        let mut suspended = HashMap::new();
        let mut download_dirs: HashMap<PathBuf, Vec<(u32, Vec<String>)>> = HashMap::new();
        let mut read_dirs = std::collections::HashSet::new();
        for (pid, _, exe, argv, paused) in found {
            suspended.insert(pid, paused);
            let dir = state_dir(&exe);
            // 每个配置目录每轮只读一次；状态文件正在写入时读取失败则沿用上一轮
            if read_dirs.insert(dir.clone())
                && let Some(entries) = read_uploads(&dir)
            {
                self.uploads.insert(dir.clone(), entries);
            }
            match argv[1].to_lowercase().as_str() {
                "upload" | "u" if argv.len() > 3 => self.collect_upload(pid, &argv, &dir),
                "download" | "d" => download_dirs
                    .entry(download_dir(&argv, &dir, &exe))
                    .or_default()
                    .push((pid, argv)),
                _ => {}
            }
        }
        for (dir, owners) in download_dirs {
            let mut sidecars = Vec::new();
            find_sidecars(&dir, &mut sidecars);
            for sidecar in sidecars {
                self.collect_download(&sidecar, &owners);
            }
        }
        for t in &mut self.tasks {
            t.paused = !t.pids.is_empty() && t.pids.iter().all(|p| suspended.get(p) == Some(&true));
        }

        self.schedule_checks(now);
        self.up_rate = 0.0;
        self.down_rate = 0.0;
        for t in &mut self.tasks {
            update_status(t, now);
            t.samples.push_back((now, t.done));
            // 上传状态文件每隔数分钟才写一次，用 10 分钟窗口求平均；下载断点文件更新频繁
            let window = if t.kind == Kind::Up { 600.0 } else { 20.0 };
            while t.samples.len() > 1 && t.samples[1].0 < now - window {
                t.samples.pop_front();
            }
            let (since, from) = t.samples[0];
            t.rate = if now - since >= 4.0 && t.running() {
                (t.done.saturating_sub(from)) as f64 / (now - since)
            } else {
                0.0
            };
            match t.kind {
                Kind::Up => self.up_rate += t.rate,
                Kind::Down => self.down_rate += t.rate,
            }
        }
        self.cloud_failed = self.tasks.iter().any(|t| t.cloud == Some("网盘查询失败"));
    }

    fn collect_upload(&mut self, pid: u32, argv: &[String], state: &Path) {
        let remote_dir = argv[argv.len() - 1].trim_end_matches('/').to_string();
        let entries = self.uploads.get(state).map(|e| {
            e.iter()
                .map(|(k, v)| (k.clone(), (v.path.clone(), v.total, v.done, v.started)))
                .collect::<Vec<_>>()
        });
        for arg in &argv[2..argv.len() - 1] {
            let source = absolute(arg);
            if !source.exists() {
                continue; // 选项值，例如 --policy skip
            }
            let prefix = key_of(&source);
            let parent = source.parent().map(Path::to_path_buf).unwrap_or_default();
            let mut matches: Vec<_> = entries
                .iter()
                .flatten()
                .filter(|(k, _)| {
                    *k == prefix || k.starts_with(&format!("{prefix}{}", std::path::MAIN_SEPARATOR))
                })
                .map(|(k, v)| (k.clone(), v.clone(), true))
                .collect();
            if source.is_file() && !matches.iter().any(|(k, _, _)| *k == prefix) {
                let size = std::fs::metadata(&source).map(|m| m.len()).unwrap_or(0);
                matches.push((prefix.clone(), (source.clone(), size, 0, false), false));
            }
            // ponytail: 上传目录时只显示已进入状态文件的文件，排队中的不预先列出
            for (_, (path, total, done, started), in_state) in matches {
                let relative = path
                    .strip_prefix(&parent)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let t = self.task(Kind::Up, path);
                t.alive = true;
                t.pids.insert(pid);
                t.command = Some(argv.to_vec());
                t.total = total;
                t.remote = Some(format!("{remote_dir}/{relative}"));
                if in_state {
                    t.active = true;
                    t.done = done.min(total);
                    t.started |= started;
                }
            }
        }
    }

    fn collect_download(&mut self, sidecar: &Path, owners: &[(u32, Vec<String>)]) {
        let Some((total, done)) = std::fs::read(sidecar)
            .ok()
            .and_then(|d| download_progress(&d))
        else {
            return;
        };
        let text = sidecar.to_string_lossy();
        let t = self.task(
            Kind::Down,
            PathBuf::from(&text[..text.len() - SIDECAR.len()]),
        );
        t.alive = true;
        t.active = true;
        t.started = true;
        // 同一保存目录的下载进程无法再细分：一起暂停／停止，续传用其中第一条命令
        t.pids.extend(owners.iter().map(|o| o.0));
        t.command = Some(owners[0].1.clone());
        t.total = total;
        t.done = done;
    }

    fn schedule_checks(&mut self, now: f64) {
        for (key, status) in self.results.lock().unwrap().drain(..) {
            if let Some(t) = self.tasks.iter_mut().find(|t| t.key == key) {
                t.cloud = Some(status);
            }
        }
        if now < self.next_check || self.checking.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let mut items = Vec::new();
        for t in &mut self.tasks {
            if t.kind != Kind::Up || t.active || t.finished() {
                continue;
            }
            let Some(remote) = t.remote.clone() else {
                continue;
            };
            if !t.alive {
                if t.dead_checked {
                    continue;
                }
                t.dead_checked = true;
            }
            // 用上传它的同一个 BaiduPCS-Go 查询，保证同一账号与配置
            let exe = t
                .command
                .as_ref()
                .map(|c| c[0].clone())
                .unwrap_or_else(|| "BaiduPCS-Go".into());
            items.push((t.key.clone(), exe, remote, t.total));
        }
        if items.is_empty() {
            return;
        }
        let results = self.results.clone();
        self.checking = Some(std::thread::spawn(move || {
            for (key, exe, remote, size) in items {
                let status = check_remote(&exe, &remote, size);
                results.lock().unwrap().push((key, status));
            }
        }));
        self.next_check = now + 30.0;
    }

    // ---------- 卡片操作 ----------

    /// 主按钮：暂停／继续，或对已中断任务续传。
    pub fn primary(&mut self, key: &str) {
        let Some(t) = self.tasks.iter().find(|t| t.key == key) else {
            return;
        };
        match t.mode() {
            Mode::Running | Mode::Paused => {
                let pause = t.mode() == Mode::Running;
                let pids: Vec<u32> = t.pids.iter().copied().collect();
                for pid in pids {
                    if platform::set_suspended(pid, pause) {
                        self.remember_pause(pid, pause);
                    }
                }
            }
            Mode::Stopped => self.restart(key),
            _ => {}
        }
        self.poll();
    }

    /// 次按钮：停止进程（之后可续传），或把已结束的任务移出列表（不删文件）。
    pub fn secondary(&mut self, key: &str) {
        let Some(index) = self.tasks.iter().position(|t| t.key == key) else {
            return;
        };
        match self.tasks[index].mode() {
            Mode::Running | Mode::Paused => {
                for pid in self.tasks[index].pids.clone() {
                    if let Some(process) = self.system.process(sysinfo::Pid::from_u32(pid)) {
                        if self.tasks[index].paused {
                            platform::set_suspended(pid, false); // Unix 上 SIGSTOP 的进程需先恢复才能结束
                        }
                        process.kill();
                    }
                }
            }
            Mode::Stopped | Mode::Failed => {
                self.tasks.remove(index);
            }
            Mode::None => {}
        }
        self.poll();
    }

    pub fn clear_finished(&mut self) {
        self.tasks.retain(|t| t.status != "已完成");
    }

    #[cfg(windows)]
    fn remember_pause(&mut self, pid: u32, pause: bool) {
        let Some(start) = self
            .system
            .process(sysinfo::Pid::from_u32(pid))
            .map(|p| p.start_time())
        else {
            return;
        };
        if pause {
            self.paused.insert((pid, start))
        } else {
            self.paused.remove(&(pid, start))
        };
        crate::store::save("paused.json", &self.paused);
    }

    #[cfg(not(windows))]
    fn remember_pause(&mut self, _: u32, _: bool) {}

    /// 在后台重新运行原命令；BaiduPCS-Go 会按上传状态文件／下载断点文件续传。
    fn restart(&mut self, key: &str) {
        let now = self.clock.elapsed().as_secs_f64();
        let Some(command) = self
            .tasks
            .iter()
            .find(|t| t.key == key)
            .and_then(|t| t.command.clone())
        else {
            return;
        };
        // 同一条命令负责的其他任务刚才已一起重启，不重复启动
        let recent = |t: &Task| t.restarted_at.is_some_and(|at| now - at < 20.0);
        if self
            .tasks
            .iter()
            .any(|t| recent(t) && t.command.as_ref() == Some(&command))
        {
            return;
        }
        let exe = PathBuf::from(&command[0]);
        let spawned = background(Command::new(&exe).args(&command[1..]))
            .current_dir(exe.parent().unwrap_or(Path::new(".")))
            .env("NO_PROXY", no_proxy())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if spawned.is_ok() {
            for t in self
                .tasks
                .iter_mut()
                .filter(|t| t.command.as_ref() == Some(&command))
            {
                t.restarted_at = Some(now);
                t.dead_checked = false;
            }
        }
    }
}

fn update_status(t: &mut Task, now: f64) {
    let starting = !t.alive && t.restarted_at.is_some_and(|at| now - at < 20.0);
    if t.kind == Kind::Down {
        if t.active {
            t.status = if t.paused { "已暂停" } else { "下载中" };
        } else if starting {
            t.status = "启动中";
        } else if t.status != "已完成" {
            let complete = std::fs::metadata(&t.path).is_ok_and(|m| m.len() == t.total);
            t.status = if complete { "已完成" } else { "已中断" };
        }
        if t.status == "已完成" {
            t.done = t.total;
        }
        return;
    }
    t.status = match t.cloud {
        Some("已完成") => {
            t.done = t.total;
            "已完成"
        }
        Some("网盘大小不符") => "网盘大小不符",
        _ if t.paused => "已暂停",
        _ if t.active && t.alive => {
            if t.started {
                "上传中"
            } else {
                "准备中"
            }
        }
        _ if t.alive => {
            if t.started {
                "等待网盘确认"
            } else {
                "排队中"
            }
        }
        _ if starting => "启动中",
        _ => "已中断",
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(mut n: u64) -> Vec<u8> {
        let mut out = Vec::new();
        while n >= 0x80 {
            out.push((n & 0x7F) as u8 | 0x80);
            n >>= 7;
        }
        out.push(n as u8);
        out
    }

    fn range(begin: u64, end: u64) -> Vec<u8> {
        let mut body = Vec::new();
        if begin != 0 {
            body.push(0x08);
            body.extend(v(begin));
        }
        body.push(0x10);
        body.extend(v(end));
        let mut out = vec![0x2A];
        out.extend(v(body.len() as u64));
        out.extend(body);
        out
    }

    #[test]
    fn real_sidecar() {
        // 真实 BaiduPCS-Go v4.0.1 断点文件（542.14MB，单区间，日志显示约 10.9MB 已下载）
        let data = [
            0x08, 0x01, 0x10, 0xa0, 0xde, 0x88, 0x8f, 0x02, 0x18, 0x80, 0x80, 0x80, 0x06, 0x20,
            0x80, 0x80, 0x80, 0x02, 0x2a, 0x0a, 0x08, 0x80, 0x80, 0xb8, 0x05, 0x10, 0x80, 0x80,
            0x80, 0x06,
        ];
        assert_eq!(download_progress(&data), Some((568_471_328, 11_403_264)));
    }

    #[test]
    fn two_ranges_and_defaults() {
        let mut data = vec![0x10];
        data.extend(v(1000));
        data.push(0x18);
        data.extend(v(600));
        data.extend(range(100, 200));
        data.extend(range(0, 50));
        assert_eq!(download_progress(&data), Some((1000, 450)));
        assert_eq!(download_progress(&[0x2A, 0x09, 0x01]), None); // 截断的数据不崩溃
    }

    #[test]
    fn meta_output() {
        let out = "\n  类型                文件\n  文件路径            /a/b c.zip\n  文件大小            3823060399, 3.56GB\n";
        assert_eq!(
            parse_meta(out),
            (Some(3_823_060_399), Some("/a/b c.zip".into()))
        );
    }

    #[test]
    fn legacy_config_next_to_exe_wins() {
        let dir = std::env::temp_dir().join(format!("btm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("BaiduPCS-Go.exe");
        assert_ne!(state_dir(&exe), dir);
        std::fs::write(dir.join("pcs_config.json"), "{}").unwrap();
        assert_eq!(state_dir(&exe), dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn suspend_and_resume_child() {
        let mut child = if cfg!(windows) {
            background(Command::new("ping").args(["-n", "30", "127.0.0.1"]))
                .stdout(Stdio::null())
                .spawn()
        } else {
            Command::new("sleep").arg("30").spawn()
        }
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(platform::set_suspended(child.id(), true));
        assert!(matches!(child.try_wait(), Ok(None)));
        assert!(platform::set_suspended(child.id(), false));
        child.kill().unwrap();
        let _ = child.wait();
    }

    #[test]
    fn durations() {
        assert_eq!(duration(3725.0), "1小时2分");
        assert_eq!(duration(65.0), "1分5秒");
    }
}
