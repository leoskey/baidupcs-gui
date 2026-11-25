# BaiduPCS GUI

[BaiduPCS-Go](https://github.com/qjfoidnh/BaiduPCS-Go)（百度网盘命令行客户端）的图形界面客户端：置顶悬浮胶囊，自动发现正在运行的 BaiduPCS-Go 上传／下载任务，显示进度并可暂停、继续、停止、续传。Rust + [gpui](https://gpui.rs)（Zed 的 GPU 界面框架），无需参数。

## 使用

- 胶囊左侧是图形标，文字显示进行中的上传、下载数量与速度；图形标右侧到胶囊右端的背景就是总体进度条（半透明上传蓝／下载绿）。可拖到任意位置自由悬浮，只有松手时完全贴住（或拖出）屏幕边缘才吸附到该边，贴边一侧变平。按下后移动不超过 6 逻辑像素算点击。位置与是否贴边记在数据目录的 `capsule.json`。
- 点击胶囊展开面板，默认选中 **下载中**，另有 **上传中 / 已完成** Tab；右上角 × 或点击别处收起。左下角是下载／上传总速度，已完成 Tab 右下角有“清除”。
- 右键胶囊弹出菜单：清除已完成、退出（退出应用只在这里）。
- 任务卡片右侧按钮：

  | 状态 | 按钮 |
  | --- | --- |
  | 进行中 | ❚❚ 暂停、✕ 停止 |
  | 已暂停 | ▷ 继续、✕ 停止 |
  | 已中断 | ↻ 续传、🗑 从列表移除（不删文件） |
  | 网盘大小不符 | 🗑 从列表移除 |

  - 暂停／继续：Windows 用 `NtSuspendProcess`／`NtResumeProcess` 冻结整个进程，macOS／Linux 用 `SIGSTOP`／`SIGCONT`；进程、所在终端、环境变量和断点状态都不变。Windows 上暂停的进程记在 `paused.json`（pid + 启动时间），监视器重启后仍识别；Unix 直接读进程状态。暂停很久后连接可能被服务器断开，恢复后由 BaiduPCS-Go 自身重试。
  - 停止：结束进程；上传状态文件和下载断点文件保留，可续传。
  - 续传：在后台（无窗口）重新运行最近一次看到的原命令，工作目录为 BaiduPCS-Go 所在目录，并把 `baidu.com,baidupcs.com` 加入 `NO_PROXY`（经本地代理上传大文件会停滞）。原命令里用相对路径的本地文件会找不到；只有监视器运行期间见过的任务才有原命令。
  - 以上操作都以进程为单位：同一条 `upload` 命令里的多个文件、保存到同一目录的下载会一起暂停／停止／续传。
- 重复启动不会出现第二个胶囊（本机端口 47613 作为单实例锁）。

数据目录：Windows `%LOCALAPPDATA%\baidupcs-gui`，macOS `~/Library/Application Support/baidupcs-gui`，Linux `$XDG_DATA_HOME`（或 `~/.local/share`）`/baidupcs-gui`。

## 数据来源

| 内容 | 来源 |
| --- | --- |
| 任务发现 | 每 1.5 秒用 `sysinfo` 枚举 `BaiduPCS-Go*` 进程并读取命令行（`upload` / `download`） |
| 配置目录 | 每个进程单独判断，规则同 BaiduPCS-Go `pcsconfig.GetConfigDir`：`BAIDUPCS_GO_CONFIG_DIR` → exe 旁已有 `pcs_config.json` 时用 exe 目录 → Windows `%APPDATA%\BaiduPCS-Go`，macOS／Linux `~/.config/BaiduPCS-Go` |
| 上传进度 | 配置目录下 `pcs_uploading.json` 中已确认分片；BaiduPCS-Go 每隔数分钟才写一次，进度会阶梯式跳变，速度按 10 分钟平均 |
| 上传完成 | 文件离开状态文件后，用上传它的同一个 BaiduPCS-Go 运行 `meta` 核对网盘大小 |
| 下载进度 | 目标文件旁的 `<文件>.BaiduPCS-Go-downloading` 断点文件（protobuf：2=总大小，3=下一个未分配偏移，5=未完成区间） |

已知不覆盖：上传整个目录时尚未开始的文件不预先列出；`download --save`（保存到进程当前目录）无法定位。

## 平台支持

| 能力 | Windows | macOS／Linux |
| --- | --- | --- |
| 任务发现、进度、暂停／继续／停止／续传 | 已实测 | 代码已实现，未编译、未实测 |
| 置顶、拖动、贴边吸附、面板定位 | 已实测 | 未实现：不置顶、不吸附；Linux 拖动交给系统（`start_window_move`），Wayland 不允许程序定位窗口 |

Windows 实现要点（`src/platform.rs`）：gpui 0.2 在 Windows 上没有置顶和“创建后移动窗口”的接口，通过原生句柄完成；gpui 创建的窗口客户区四周有不可见边框，定位一律按客户区；窗口改为 `WS_POPUP` 并关闭 DWM 描边与圆角，形状完全由 gpui 绘制；`SetWindowPos` 必须在 gpui 更新回调之外调用（`Capsule::native_later`），否则 gpui 收不到尺寸变化，内容会被截断。

## 构建与验证

```powershell
cargo build --release
.\target\release\baidupcs-gui.exe    # 运行
cargo test --release                           # 断点文件解析、meta 输出、配置目录规则、真实暂停／恢复子进程
```

首次构建 gpui 约 5 分钟。胶囊运行时 exe 被占用，重新构建前先在面板点“退出”。Windows 下 exe 图标来自 `branding/app-icon.ico`（`build.rs` 嵌入；图形标与导出方法见 [branding/README.md](branding/README.md)）。
