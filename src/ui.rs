//! 界面：置顶悬浮胶囊（拖动、靠近边缘才吸附）+ 点击展开的 Tab 面板（下载中／上传中／已完成）。

use std::borrow::Cow;
use std::time::{Duration, Instant};

use gpui::{
    App, AssetSource, Bounds, Context, Entity, FontWeight, Hsla, MouseButton, SharedString,
    Subscription, TextRun, WeakEntity, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, div, font, img, point, prelude::*, px, relative, rgb,
    size, svg,
};
use serde::{Deserialize, Serialize};

use crate::monitor::{Kind, Mode, Monitor, Tab, Task};
use crate::platform::{self, Rect};
use crate::store;

#[cfg(windows)]
const FONT: &str = "Microsoft YaHei UI";
#[cfg(target_os = "macos")]
const FONT: &str = "PingFang SC";
#[cfg(not(any(windows, target_os = "macos")))]
const FONT: &str = "Noto Sans CJK SC";

const MIB: f64 = 1024.0 * 1024.0;
const CAPSULE_H: f32 = 40.0;
const RADIUS: f32 = 20.0;
const PANEL_W: f32 = 460.0;
const CARD_H: f32 = 78.0;
const VISIBLE_CARDS: usize = 5;

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}
fn bg() -> Hsla {
    color(0x111418)
}
fn card() -> Hsla {
    color(0x1B2027)
}
fn card_hover() -> Hsla {
    color(0x232A33)
}
fn track() -> Hsla {
    color(0x2A313A)
}
fn line() -> Hsla {
    color(0x2F3742)
}
fn ink() -> Hsla {
    color(0xE8ECF1)
}
fn muted() -> Hsla {
    color(0x8A95A3)
}
fn up() -> Hsla {
    color(0x4C9EFF)
}
fn down() -> Hsla {
    color(0x3DDC97)
}
fn warn() -> Hsla {
    color(0xF5A524)
}
fn err() -> Hsla {
    color(0xF2555A)
}

fn accent(t: &Task) -> Hsla {
    match t.status {
        "已完成" => down(),
        "网盘大小不符" => err(),
        "已中断" => warn(),
        "已暂停" => muted(),
        _ if t.kind == Kind::Up => up(),
        _ => down(),
    }
}

// ---------- 图标资源 ----------

pub struct Assets;

const ICONS: &[(&str, &str)] = &[
    ("icons/pause.svg", include_str!("../assets/icons/pause.svg")),
    ("icons/play.svg", include_str!("../assets/icons/play.svg")),
    ("icons/stop.svg", include_str!("../assets/icons/stop.svg")),
    (
        "icons/restart.svg",
        include_str!("../assets/icons/restart.svg"),
    ),
    (
        "icons/remove.svg",
        include_str!("../assets/icons/remove.svg"),
    ),
];

/// 品牌图形标（蓝绿渐变）：用 img() 渲染保留颜色，svg() 只会当作单色遮罩
const LOGO: &str = "brand/mark-color.svg";
const LOGO_CAPSULE: f32 = 20.0;
const LOGO_GAP: f32 = 8.0;
/// 胶囊背景进度的起点：左内边距 18 + 图形标 + 半个间距（相对描边内侧）
const FILL_START: f32 = 18.0 + LOGO_CAPSULE + LOGO_GAP / 2.0;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO {
            // 图形标画布四周留白较多（为应用图标准备）；界面内裁成贴合云朵的正方形，小尺寸下更清楚
            let svg = include_str!("../branding/mark-color.svg")
                .replace(r#"viewBox="0 0 100 100""#, r#"viewBox="15 15.5 70 70""#);
            return Ok(Some(Cow::Owned(svg.into_bytes())));
        }
        Ok(ICONS
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, svg)| Cow::Borrowed(svg.as_bytes())))
    }
    fn list(&self, _: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS.iter().map(|(p, _)| SharedString::from(*p)).collect())
    }
}

// ---------- 胶囊 ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge {
    Left,
    Top,
    Right,
    Bottom,
    /// 自由悬浮
    None,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    edge: Edge,
    x: i32,
    y: i32,
}

pub struct Capsule {
    monitor: Entity<Monitor>,
    hwnd: Option<isize>,
    edge: Edge,
    placed: bool,
    width: f32,
    scale: f32,
    pressed: bool,
    /// 已安排一次尺寸校正，等它完成（moved）前不重复安排
    fixing: bool,
    drag: Option<gpui::Task<()>>,
    slide: Option<gpui::Task<()>>,
    panel: Option<WindowHandle<Panel>>,
    panel_hwnd: Option<isize>,
    panel_closed_at: Option<Instant>,
    _subscriptions: Vec<Subscription>,
}

pub fn open_capsule(monitor: Entity<Monitor>, cx: &mut App) -> WindowHandle<Capsule> {
    let display = cx.primary_display().map(|d| d.bounds());
    let origin = display.map_or(point(px(100.), px(100.)), |b| {
        b.bottom_right() - point(px(240.), px(160.))
    });
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin,
            size: size(px(200.), px(CAPSULE_H)),
        })),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("baidupcs-gui".into()),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        cx.new(|cx| Capsule::new(monitor, window, cx))
    })
    .expect("open capsule window")
}

impl Capsule {
    fn new(monitor: Entity<Monitor>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let hwnd = platform::native_handle(window);
        if let Some(h) = hwnd {
            platform::set_topmost(h);
        }
        let this = cx.entity();
        window.on_next_frame(move |window, cx| this.update(cx, |c, cx| c.restore(window, cx)));
        Capsule {
            _subscriptions: vec![cx.observe(&monitor, |_, _, cx| cx.notify())],
            monitor,
            hwnd,
            edge: Edge::Right,
            placed: false,
            width: 200.0,
            scale: window.scale_factor(),
            pressed: false,
            fixing: false,
            drag: None,
            slide: None,
            panel: None,
            panel_hwnd: None,
            panel_closed_at: None,
        }
    }

    fn rect(&self) -> Option<Rect> {
        self.hwnd.and_then(platform::window_rect)
    }

    /// 在本次 gpui 更新结束后执行原生窗口操作，再回到胶囊继续。
    /// 在更新回调里同步调用 SetWindowPos 时 App 正被借用，gpui 对随之而来的 WM_SIZE／WM_MOVE
    /// 的处理会被丢弃：窗口尺寸变了，绘制尺寸却没变（内容被截断）。
    fn native_later(
        cx: &mut Context<Self>,
        op: impl FnOnce() + 'static,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            op();
            this.update(cx, |c, cx| then(c, cx)).ok();
        })
        .detach();
    }

    /// 恢复上次位置；没有或已不在任何屏幕上时放到主屏右侧偏下。
    fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.scale = window.scale_factor();
        let (Some(h), Some(r)) = (self.hwnd, self.rect()) else {
            return;
        };
        let saved: Option<Saved> = store::load("capsule.json");
        let (x, y, edge) = match saved {
            Some(s) if platform::on_any_screen(s.x + r.width() / 2, s.y + r.height() / 2) => {
                (s.x, s.y, s.edge)
            }
            _ => match platform::work_area(0, 0) {
                Some(a) => (
                    a.right - r.width(),
                    a.bottom - r.height() - (120.0 * self.scale) as i32,
                    Edge::Right,
                ),
                None => (r.left, r.top, Edge::Right),
            },
        };
        Self::native_later(
            cx,
            move || platform::move_to(h, x, y),
            move |c, cx| {
                c.placed = true;
                c.snap(Some(edge), false, cx);
            },
        );
    }

    /// 贴到 target 边；None 时（拖动松手）只有离某条边不超过 SNAP_DISTANCE 或已拖出屏幕才吸附，否则原地悬浮。
    fn snap(&mut self, target: Option<Edge>, animate: bool, cx: &mut Context<Self>) {
        let (Some(h), Some(r)) = (self.hwnd, self.rect()) else {
            return;
        };
        let (cx_, cy_) = r.center();
        let Some(a) = platform::work_area(cx_, cy_) else {
            return;
        };
        // 跨屏拖动后缩放比例会变：每次按当前屏幕重新取，并把窗口大小校正为该缩放下的应有值
        // （Windows／gpui 处理跨屏 DPI 变化时偶尔多缩放一次，吸附时一并纠正）
        self.scale = platform::dpi_scale(h).unwrap_or(self.scale);
        let w = (self.width * self.scale).round() as i32;
        let hgt = (CAPSULE_H * self.scale).round() as i32;
        let r = Rect {
            right: r.left + w,
            bottom: r.top + hgt,
            ..r
        };
        self.edge = target.unwrap_or_else(|| {
            let (nearest, distance) = [
                (Edge::Left, r.left - a.left),
                (Edge::Right, a.right - r.right),
                (Edge::Top, r.top - a.top),
                (Edge::Bottom, a.bottom - r.bottom),
            ]
            .into_iter()
            .min_by_key(|p| p.1)
            .unwrap();
            // 只有完全贴住（或拖出）屏幕边缘才吸附
            if distance <= 0 { nearest } else { Edge::None }
        });
        let clamp = |v: i32, lo: i32, hi: i32| v.clamp(lo, hi.max(lo));
        let x = match self.edge {
            Edge::Left => a.left,
            Edge::Right => a.right - r.width(),
            _ => clamp(r.left, a.left, a.right - r.width()),
        };
        let y = match self.edge {
            Edge::Top => a.top,
            Edge::Bottom => a.bottom - r.height(),
            _ => clamp(r.top, a.top, a.bottom - r.height()),
        };
        cx.notify(); // 贴边一侧变平
        let frame = Rect {
            left: x,
            top: y,
            right: x + w,
            bottom: y + hgt,
        };
        if !animate || (x == r.left && y == r.top) {
            Self::native_later(
                cx,
                move || platform::set_frame(h, frame),
                |c, cx| c.moved(cx),
            );
            return;
        }
        // 12 帧 ease-out 滑动到边缘，最后一帧同时校正大小
        self.slide = Some(cx.spawn(async move |this, cx| {
            for step in 1..=12 {
                cx.background_executor()
                    .timer(Duration::from_millis(12))
                    .await;
                let k = 1.0 - (1.0 - step as f32 / 12.0).powi(3);
                platform::move_to(
                    h,
                    r.left + ((x - r.left) as f32 * k).round() as i32,
                    r.top + ((y - r.top) as f32 * k).round() as i32,
                );
            }
            platform::set_frame(h, frame);
            this.update(cx, |c, cx| c.moved(cx)).ok();
        }));
    }

    fn moved(&mut self, cx: &mut Context<Self>) {
        self.fixing = false;
        if let Some(r) = self.rect() {
            store::save(
                "capsule.json",
                &Saved {
                    edge: self.edge,
                    x: r.left,
                    y: r.top,
                },
            );
        }
        self.place_panel(cx);
    }

    /// 内容变宽／变窄：按贴边方向保持边缘不动（Windows），其他平台只改大小。
    fn apply_width(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.width = width;
        self.scale = window.scale_factor();
        let (Some(h), Some(r)) = (self.hwnd, self.rect()) else {
            window.resize(size(px(width), px(CAPSULE_H)));
            return;
        };
        let w = (width * self.scale).round() as i32;
        let left = if self.edge == Edge::Right {
            r.right - w
        } else {
            r.left
        };
        let frame = Rect {
            left,
            top: r.top,
            right: left + w,
            bottom: r.top + (CAPSULE_H * self.scale).round() as i32,
        };
        Self::native_later(
            cx,
            move || platform::set_frame(h, frame),
            |c, cx| {
                if c.placed {
                    c.snap(Some(c.edge), false, cx);
                }
            },
        );
    }

    fn on_press(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(h), Some(start), Some(r)) = (self.hwnd, platform::cursor(), self.rect()) else {
            self.pressed = true; // 其他平台：松开为点击，按住移动交给系统拖动
            return;
        };
        // 移动超过 6 逻辑像素才算拖动：点击时手抖几个像素仍算点击（150% 缩放下 4 个物理像素太容易超过）
        let threshold = (6.0 * platform::dpi_scale(h).unwrap_or(1.0)).round() as i32;
        // 轮询全局光标移动窗口，避免在 gpui 事件回调里进入系统模态拖动循环
        self.drag = Some(cx.spawn_in(window, async move |this, cx| {
            let mut moved = false;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(8))
                    .await;
                let Some((x, y)) = platform::cursor() else {
                    break;
                };
                let (dx, dy) = (x - start.0, y - start.1);
                if moved || dx.abs().max(dy.abs()) > threshold {
                    moved = true;
                    platform::move_to(h, r.left + dx, r.top + dy);
                }
                if !platform::left_button_down() {
                    break;
                }
            }
            this.update_in(cx, |c, window, cx| {
                c.drag = None;
                if moved {
                    c.snap(None, true, cx)
                } else {
                    c.toggle_panel(window, cx)
                }
            })
            .ok();
        }));
    }

    fn toggle_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 面板仍在：本次点击即收起。窗口已不存在（句柄过期）则当作已关闭，继续打开新面板
        if let Some(panel) = self.panel.take() {
            self.panel_hwnd = None;
            if panel
                .update(cx, |_, window, _| window.remove_window())
                .is_ok()
            {
                return;
            }
        }
        // 本次点击刚让面板失焦关闭：视为“收起”
        if self
            .panel_closed_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(400))
        {
            return;
        }
        let height = Panel::height(self.monitor.read(cx));
        // 先在胶囊自己的位置打开（一定在屏幕上），随后由 place_panel 设定最终位置和大小
        let origin = window.bounds().origin;
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin,
                size: size(px(PANEL_W), px(height)),
            })),
            titlebar: None,
            focus: true,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some("baidupcs-gui".into()),
            ..Default::default()
        };
        let monitor = self.monitor.clone();
        let capsule = cx.entity().downgrade();
        self.panel = cx
            .open_window(options, |window, cx| {
                cx.new(|cx| Panel::new(monitor, capsule, window, cx))
            })
            .ok();
    }

    /// 面板出现在胶囊远离屏幕边缘的一侧；悬浮时在上半屏向下展开，否则向上。
    /// 位置和大小一起设置：gpui 打开窗口时若请求的位置不在屏幕上，会改用默认大小（屏幕的 60%）。
    fn place_panel(&self, cx: &mut Context<Self>) {
        let (Some(p), Some(c)) = (self.panel_hwnd, self.rect()) else {
            return;
        };
        let Some(a) = platform::work_area(c.center().0, c.center().1) else {
            return;
        };
        let height = Panel::height(self.monitor.read(cx));
        // 按胶囊当前所在屏幕的缩放计算面板大小（面板会放到同一屏幕上）
        let scale = self
            .hwnd
            .and_then(platform::dpi_scale)
            .unwrap_or(self.scale);
        let pw = (PANEL_W * scale).round() as i32;
        let ph = (height * scale).round() as i32;
        let mid_x = (c.left + c.right - pw) / 2;
        let mid_y = (c.top + c.bottom - ph) / 2;
        let (x, y) = match self.edge {
            Edge::Left => (c.right, mid_y),
            Edge::Right => (c.left - pw, mid_y),
            Edge::Top => (mid_x, c.bottom),
            Edge::Bottom => (mid_x, c.top - ph),
            Edge::None if c.top + c.bottom < a.top + a.bottom => (mid_x, c.bottom),
            Edge::None => (mid_x, c.top - ph),
        };
        let (x, y) = (
            x.clamp(a.left, (a.right - pw).max(a.left)),
            y.clamp(a.top, (a.bottom - ph).max(a.top)),
        );
        let frame = Rect {
            left: x,
            top: y,
            right: x + pw,
            bottom: y + ph,
        };
        Self::native_later(cx, move || place_on_screen(p, frame), |_, _| {});
    }

    /// 右键菜单：在光标处打开（清除已完成、退出）；先收起面板。
    fn open_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.panel.take() {
            panel.update(cx, |_, window, _| window.remove_window()).ok();
            self.panel_hwnd = None;
            // 面板销毁后 Windows 会把焦点交给别的窗口；菜单失焦即关，所以等面板关完再打开菜单
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                this.update(cx, |c, cx| c.open_menu(cx)).ok();
            })
            .detach();
            return;
        }
        let at = platform::cursor();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(MENU_W), px(menu_height())),
            })),
            titlebar: None,
            focus: true,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some("baidupcs-gui".into()),
            ..Default::default()
        };
        let monitor = self.monitor.clone();
        cx.open_window(options, |window, cx| {
            cx.new(|cx| Menu::new(monitor, at, window, cx))
        })
        .ok();
    }

    /// 托盘调用：隐藏／显示胶囊；隐藏时一并收起面板。
    pub fn toggle_visible(&mut self, cx: &mut Context<Self>) {
        let Some(h) = self.hwnd else {
            return;
        };
        if let Some(panel) = self.panel.take() {
            panel.update(cx, |_, window, _| window.remove_window()).ok();
            self.panel_hwnd = None;
        }
        let show = !platform::is_visible(h);
        Self::native_later(cx, move || platform::set_visible(h, show), |_, _| {});
    }

    fn panel_opened(&mut self, hwnd: Option<isize>, cx: &mut Context<Self>) {
        self.panel_hwnd = hwnd;
        self.place_panel(cx);
    }

    fn panel_closed(&mut self) {
        self.panel = None;
        self.panel_hwnd = None;
        self.panel_closed_at = Some(Instant::now());
    }
}

/// 把窗口放到目标屏幕上的 frame（物理像素）。先移动：跨到不同缩放的屏幕时 Windows 会发 DPI 变化，
/// gpui 按建议尺寸把窗口再缩放一次；等这一步完成后再设定最终大小，结果才与目标屏幕的缩放一致。
fn place_on_screen(h: isize, frame: Rect) {
    platform::move_to(h, frame.left, frame.top);
    platform::set_frame(h, frame);
}

/// 胶囊文字：(文字, 颜色) 片段 + 进行中任务的总体进度。
fn summary(m: &Monitor) -> (Vec<(String, Hsla)>, Option<(f32, Hsla)>) {
    let active: Vec<&Task> = m
        .tasks
        .iter()
        .filter(|t| t.tab() != Tab::Done && !matches!(t.status, "已中断" | "已暂停"))
        .collect();
    let ups = active.iter().filter(|t| t.kind == Kind::Up).count();
    let downs = active.len() - ups;
    let done = m.tasks.iter().filter(|t| t.tab() == Tab::Done).count();
    let stopped = m.tasks.iter().filter(|t| t.status == "已中断").count();
    let paused = m.tasks.iter().filter(|t| t.status == "已暂停").count();
    let mut out: Vec<(String, Hsla)> = Vec::new();
    if ups > 0 {
        out.push(("↑".into(), up()));
        out.push((format!(" {ups}  {:.1} MB/s", m.up_rate / MIB), ink()));
    }
    if downs > 0 {
        out.push((if ups > 0 { "    ↓" } else { "↓" }.into(), down()));
        out.push((format!(" {downs}  {:.1} MB/s", m.down_rate / MIB), ink()));
    }
    if active.is_empty() && paused == 0 && stopped == 0 {
        if done > 0 {
            out.push(("✓".into(), down()));
            out.push((format!(" {done} 个已完成"), ink()));
        } else {
            out.push(("● 网盘传输空闲".into(), muted()));
        }
    }
    let gap = |out: &Vec<(String, Hsla)>| if out.is_empty() { "" } else { "    " };
    if paused > 0 {
        out.push((format!("{}❚❚ {paused} 个暂停", gap(&out)), muted()));
    }
    if stopped > 0 {
        out.push((format!("{}⚠ {stopped} 个中断", gap(&out)), warn()));
    }
    let total: u64 = active.iter().map(|t| t.total).sum();
    let progress = (total > 0).then(|| {
        let done: u64 = active.iter().map(|t| t.done).sum();
        (
            done as f32 / total as f32,
            if ups > 0 { up() } else { down() },
        )
    });
    (out, progress)
}

impl Render for Capsule {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (segments, progress) = summary(self.monitor.read(cx));

        // 按文字实际宽度调整窗口宽度
        let bold = gpui::Font {
            weight: FontWeight::SEMIBOLD,
            ..font(FONT)
        };
        let measure = |text: String| -> f32 {
            let run = TextRun {
                len: text.len(),
                font: bold.clone(),
                color: ink(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(
                window
                    .text_system()
                    .shape_line(text.into(), px(13.), &[run], None)
                    .width,
            )
        };
        let text: String = segments.iter().map(|s| s.0.as_str()).collect();
        // 文字 + 左侧图形标 + 左右内边距与描边
        let width = (measure(text) + LOGO_CAPSULE + LOGO_GAP + 38.0).ceil();
        if (width - self.width).abs() > 0.5 {
            let this = cx.entity();
            window.on_next_frame(move |window, cx| {
                this.update(cx, |c, cx| c.apply_width(width, window, cx))
            });
        } else if self.placed && !self.fixing && self.drag.is_none() {
            // 自我校正：实际窗口尺寸与当前屏幕缩放下的应有尺寸不符时重新吸附定位。
            // Windows 对新建窗口或跨屏窗口的 DPI 变化通知可能晚于我们的定位到达，gpui 收到后会再缩放一次。
            if let (Some(r), Some(scale)) = (self.rect(), self.hwnd.and_then(platform::dpi_scale)) {
                let expected_w = (self.width * scale).round() as i32;
                let expected_h = (CAPSULE_H * scale).round() as i32;
                if (r.width() - expected_w).abs() > 1 || (r.height() - expected_h).abs() > 1 {
                    self.fixing = true;
                    let edge = self.edge;
                    cx.spawn(async move |this, cx| {
                        this.update(cx, |c, cx| c.snap(Some(edge), false, cx)).ok();
                    })
                    .detach();
                }
            }
        }

        let r = px(RADIUS);
        let pill = div()
            .id("capsule")
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .px(px(18.))
            .bg(card())
            .hover(|s| s.bg(card_hover()))
            .border_color(line())
            .cursor_pointer()
            .font_family(FONT)
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(13.));
        // 贴边一侧变平、去掉该侧描边；悬浮时为完整药丸形
        let pill = match self.edge {
            Edge::Left => pill.border_t_1().border_b_1().border_r_1(),
            Edge::Right => pill.border_t_1().border_b_1().border_l_1(),
            Edge::Top => pill.border_l_1().border_r_1().border_b_1(),
            Edge::Bottom => pill.border_l_1().border_r_1().border_t_1(),
            Edge::None => pill.border_1(),
        };
        let pill = rounded_for(pill, self.edge, r);
        // 背景即进度条：从图形标右侧到胶囊右端，按总体进度填充半透明的上传蓝／下载绿，文字在其上。
        // 填满时右端跟随胶囊右侧圆角（gpui 裁剪不跟随圆角，填充层自己带圆角，半径减去 1px 描边）
        let fill = progress.map(|(fraction, c)| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(FILL_START))
                .right_0()
                .child(fill_corners(
                    div().h_full().w(relative(fraction)).bg(c.alpha(0.28)),
                    self.edge,
                    px(RADIUS - 1.0),
                    fraction > 0.97,
                ))
        });
        pill.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| this.on_press(window, cx)),
        )
        .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, window, _| {
            if this.pressed && ev.pressed_button == Some(MouseButton::Left) {
                this.pressed = false;
                window.start_window_move();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                if std::mem::take(&mut this.pressed) {
                    this.toggle_panel(window, cx);
                }
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, _, _, cx| this.open_menu(cx)),
        )
        .children(fill)
        .child(
            img(LOGO)
                .flex_none()
                .size(px(LOGO_CAPSULE))
                .mr(px(LOGO_GAP)),
        )
        .child(
            div().flex().flex_row().whitespace_nowrap().children(
                segments
                    .into_iter()
                    .map(|(t, c)| div().text_color(c).child(t)),
            ),
        )
    }
}

/// 进度填充层的圆角：左端从图形标右侧开始，是直边；右端是进度前沿，也保持直边，
/// 接近填满（`full`）时才跟随胶囊右侧圆角。
fn fill_corners<E: Styled>(element: E, edge: Edge, r: gpui::Pixels, full: bool) -> E {
    // 胶囊右侧哪些角是圆的
    let (tr, br) = match edge {
        Edge::Left | Edge::None => (true, true),
        Edge::Right => (false, false),
        Edge::Top => (false, true),
        Edge::Bottom => (true, false),
    };
    let element = if full && tr {
        element.rounded_tr(r)
    } else {
        element
    };
    if full && br {
        element.rounded_br(r)
    } else {
        element
    }
}

/// 按贴边方向设置圆角：贴边一侧为直角，其余为 r；悬浮时四角都是 r。
fn rounded_for<E: Styled>(element: E, edge: Edge, r: gpui::Pixels) -> E {
    match edge {
        Edge::Left => element.rounded_r(r),
        Edge::Right => element.rounded_l(r),
        Edge::Top => element.rounded_b(r),
        Edge::Bottom => element.rounded_t(r),
        Edge::None => element.rounded(r),
    }
}

// ---------- 面板 ----------

pub struct Panel {
    monitor: Entity<Monitor>,
    capsule: WeakEntity<Capsule>,
    hwnd: Option<isize>,
    height: f32,
    _subscriptions: Vec<Subscription>,
}

impl Panel {
    fn new(
        monitor: Entity<Monitor>,
        capsule: WeakEntity<Capsule>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let hwnd = platform::native_handle(window);
        if let Some(h) = hwnd {
            platform::set_topmost(h);
        }
        // 胶囊此刻仍在处理点击，下一帧再请它定位面板
        let owner = capsule.clone();
        window.on_next_frame(move |_, cx| {
            owner.update(cx, |c, cx| c.panel_opened(hwnd, cx)).ok();
        });
        let owner = capsule.clone();
        let deactivated = cx.observe_window_activation(window, move |_, window, cx| {
            if !window.is_window_active() {
                // 失焦往往由点击胶囊引起，此刻胶囊正在处理事件、无法更新；改在本次更新结束后通知
                let owner = owner.clone();
                cx.spawn(async move |_, cx| {
                    owner.update(cx, |c, _| c.panel_closed()).ok();
                })
                .detach();
                window.remove_window();
            }
        });
        Panel {
            hwnd,
            height: Self::height(monitor.read(cx)),
            _subscriptions: vec![cx.observe(&monitor, |_, _, cx| cx.notify()), deactivated],
            monitor,
            capsule,
        }
    }

    /// 面板窗口高度：外边距 12（留给阴影）+ 内边距 + 标题 + Tab + 列表（最多 5 张卡片，超出滚动）+ 页脚。
    fn height(m: &Monitor) -> f32 {
        let n = m
            .tasks
            .iter()
            .filter(|t| t.tab() == m.tab)
            .count()
            .min(VISIBLE_CARDS);
        let list = if n == 0 {
            60.0
        } else {
            n as f32 * (CARD_H + 8.0) - 8.0
        };
        12.0 * 2.0 + 14.0 * 2.0 + 24.0 + 12.0 + 36.0 + 12.0 + list + 10.0 + 18.0
    }
}

// 构建函数不借用任何外部数据（`use<>`），这样可以在读取 Monitor 的同时生成元素
fn tab_button(
    monitor: &Entity<Monitor>,
    tab: Tab,
    label: &'static str,
    count: usize,
    selected: bool,
) -> impl IntoElement + use<> {
    let monitor = monitor.clone();
    div()
        .id(label)
        .flex_1()
        .h(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .cursor_pointer()
        .text_size(px(13.))
        .when(selected, |d| {
            d.bg(track())
                .text_color(ink())
                .font_weight(FontWeight::SEMIBOLD)
        })
        .when(!selected, |d| {
            d.text_color(muted()).hover(|s| s.text_color(ink()))
        })
        .child(format!("{label}  {count}"))
        .on_click(move |_, _, cx| {
            monitor.update(cx, |m, cx| {
                m.tab = tab;
                cx.notify();
            })
        })
}

fn icon_button(
    monitor: &Entity<Monitor>,
    id: (&'static str, usize),
    icon: &'static str,
    key: String,
    primary: bool,
) -> impl IntoElement + use<> {
    let monitor = monitor.clone();
    div()
        .id(id)
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(|s| s.bg(track()))
        .child(svg().path(icon).size(px(12.)).text_color(muted()))
        .on_click(move |_, _, cx| {
            monitor.update(cx, |m, cx| {
                if primary {
                    m.primary(&key)
                } else {
                    m.secondary(&key)
                }
                cx.notify();
            })
        })
}

fn task_card(monitor: &Entity<Monitor>, index: usize, t: &Task) -> impl IntoElement + use<> {
    let mode = t.mode();
    let primary = match mode {
        Mode::Running => Some("icons/pause.svg"),
        Mode::Paused => Some("icons/play.svg"),
        Mode::Stopped if t.can_restart() => Some("icons/restart.svg"),
        _ => None,
    };
    let secondary = match mode {
        Mode::Running | Mode::Paused => Some("icons/stop.svg"),
        Mode::Stopped | Mode::Failed => Some("icons/remove.svg"),
        Mode::None => None,
    };
    let a = accent(t);
    let detail_color = if matches!(t.status, "已中断" | "网盘大小不符") {
        a
    } else {
        muted()
    };
    div()
        .h(px(CARD_H))
        .flex_none()
        .flex()
        .flex_col()
        .px(px(14.))
        .py(px(10.))
        .rounded(px(10.))
        .bg(card())
        .child(
            div()
                .h(px(20.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(if t.kind == Kind::Up { up() } else { down() })
                        .child(if t.kind == Kind::Up { "↑" } else { "↓" }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ink())
                        .child(t.name()),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if t.running() { ink() } else { a })
                        .child(format!("{:.1}%", t.fraction() * 100.0)),
                )
                .when_some(primary, |row, icon| {
                    row.child(icon_button(
                        monitor,
                        ("primary", index),
                        icon,
                        t.key().into(),
                        true,
                    ))
                })
                .when_some(secondary, |row, icon| {
                    row.child(icon_button(
                        monitor,
                        ("secondary", index),
                        icon,
                        t.key().into(),
                        false,
                    ))
                }),
        )
        .child(
            div()
                .mt(px(9.))
                .mb(px(7.))
                .h(px(6.))
                .w_full()
                .rounded(px(3.))
                .bg(track())
                .child(
                    div()
                        .h_full()
                        .w(relative(t.fraction()))
                        .rounded(px(3.))
                        .bg(a),
                ),
        )
        .child(
            div()
                .h(px(16.))
                .truncate()
                .text_size(px(11.5))
                .text_color(detail_color)
                .child(t.detail()),
        )
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 任务数量变化，或实际尺寸与所在屏幕缩放下的应有尺寸不符（DPI 变化通知迟到时 gpui 会再缩放一次）：
        // 请胶囊重新定位并设定大小（在本次更新结束后执行）
        let height = Self::height(self.monitor.read(cx));
        let wrong_size = self
            .hwnd
            .and_then(|h| Some((platform::window_rect(h)?, platform::dpi_scale(h)?)))
            .is_some_and(|(r, scale)| {
                (r.width() - (PANEL_W * scale).round() as i32).abs() > 1
                    || (r.height() - (height * scale).round() as i32).abs() > 1
            });
        if (height - self.height).abs() > 0.5 || wrong_size {
            self.height = height;
            if self.hwnd.is_some() {
                let capsule = self.capsule.clone();
                cx.spawn(async move |_, cx| {
                    capsule.update(cx, |c, cx| c.place_panel(cx)).ok();
                })
                .detach();
            } else {
                window.resize(size(px(PANEL_W), px(height)));
            }
        }

        let m = self.monitor.read(cx);
        let count = |tab| m.tasks.iter().filter(|t| t.tab() == tab).count();
        let (n_down, n_up, n_done) = (count(Tab::Down), count(Tab::Up), count(Tab::Done));
        let tab = m.tab;
        let speed = format!(
            "↓ {:.1} MB/s    ↑ {:.1} MB/s",
            m.down_rate / MIB,
            m.up_rate / MIB
        );
        let cloud_failed = m.cloud_failed;
        let monitor = self.monitor.clone();
        let capsule = self.capsule.clone();
        let cards: Vec<_> = m
            .tasks
            .iter()
            .filter(|t| t.tab() == tab)
            .enumerate()
            .map(|(i, t)| task_card(&monitor, i, t))
            .collect();
        let empty = cards.is_empty();

        div().size_full().p(px(12.)).font_family(FONT).child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .px(px(16.))
                .py(px(14.))
                .bg(bg())
                .border_1()
                .border_color(line())
                .rounded(px(14.))
                .shadow_lg()
                .child(
                    div()
                        .h(px(24.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .child(img(LOGO).flex_none().size(px(22.)))
                                .child(
                                    div()
                                        .text_size(px(16.))
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(ink())
                                        .child("百度网盘传输"),
                                ),
                        )
                        // 右上角关闭：只收起面板，退出应用在胶囊右键菜单里
                        .child(
                            div()
                                .id("close")
                                .size(px(26.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .cursor_pointer()
                                .hover(|s| s.bg(track()))
                                .child(
                                    svg()
                                        .path("icons/stop.svg")
                                        .size(px(12.))
                                        .text_color(muted()),
                                )
                                .on_click(move |_, window, cx| {
                                    capsule.update(cx, |c, _| c.panel_closed()).ok();
                                    window.remove_window();
                                }),
                        ),
                )
                .child(
                    div()
                        .mt(px(12.))
                        .h(px(36.))
                        .p(px(3.))
                        .flex()
                        .flex_row()
                        .rounded(px(9.))
                        .bg(card())
                        .child(tab_button(
                            &monitor,
                            Tab::Down,
                            "下载中",
                            n_down,
                            tab == Tab::Down,
                        ))
                        .child(tab_button(
                            &monitor,
                            Tab::Up,
                            "上传中",
                            n_up,
                            tab == Tab::Up,
                        ))
                        .child(tab_button(
                            &monitor,
                            Tab::Done,
                            "已完成",
                            n_done,
                            tab == Tab::Done,
                        )),
                )
                .child(
                    div()
                        .id("list")
                        .mt(px(12.))
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .overflow_y_scroll()
                        .when(empty, |d| {
                            d.items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .text_color(muted())
                                .child("暂无任务")
                        })
                        .children(cards),
                )
                .child(
                    div()
                        .mt(px(10.))
                        .h(px(18.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(14.))
                        .text_size(px(12.))
                        // 左下角：下载／上传总速度；网盘核对失败时附带提示
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .flex()
                                .flex_row()
                                .gap(px(14.))
                                .child(div().text_color(muted()).child(speed))
                                .when(cloud_failed, |row| {
                                    row.child(
                                        div()
                                            .text_color(warn())
                                            .child("网盘查询失败，稍后自动重试"),
                                    )
                                }),
                        )
                        .when(tab == Tab::Done && !empty, |row| {
                            let monitor = monitor.clone();
                            row.child(
                                div()
                                    .id("clear")
                                    .cursor_pointer()
                                    .text_color(up())
                                    .child("清除")
                                    .on_click(move |_, _, cx| {
                                        monitor.update(cx, |m, cx| {
                                            m.clear_finished();
                                            cx.notify();
                                        })
                                    }),
                            )
                        }),
                ),
        )
    }
}

// ---------- 右键菜单 ----------

const MENU_W: f32 = 160.0;
const MENU_ITEM_H: f32 = 32.0;
const MENU_ITEMS: usize = 2;
/// 外边距 8（留给阴影）+ 内边距 4
fn menu_height() -> f32 {
    8.0 * 2.0 + 4.0 * 2.0 + MENU_ITEM_H * MENU_ITEMS as f32
}

/// 胶囊右键菜单：独立的置顶小窗口，出现在光标处，失焦即关闭。
pub struct Menu {
    monitor: Entity<Monitor>,
    _subscriptions: Vec<Subscription>,
}

impl Menu {
    fn new(
        monitor: Entity<Monitor>,
        at: Option<(i32, i32)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let hwnd = platform::native_handle(window);
        if let (Some(h), Some((x, y))) = (hwnd, at) {
            platform::set_topmost(h);
            // 在本次更新结束后移到光标所在屏幕，再按该屏缩放设定大小（与面板同理），并保持在屏幕内
            cx.spawn(async move |_, _| {
                platform::move_to(h, x, y);
                let (Some(scale), Some(a)) = (platform::dpi_scale(h), platform::work_area(x, y))
                else {
                    return;
                };
                let (w, hgt) = (
                    (MENU_W * scale).round() as i32,
                    (menu_height() * scale).round() as i32,
                );
                let x = x.min(a.right - w).max(a.left);
                let y = if y + hgt > a.bottom { y - hgt } else { y }.max(a.top);
                place_on_screen(
                    h,
                    Rect {
                        left: x,
                        top: y,
                        right: x + w,
                        bottom: y + hgt,
                    },
                );
            })
            .detach();
        }
        let closed = cx.observe_window_activation(window, |_, window, _| {
            if !window.is_window_active() {
                window.remove_window();
            }
        });
        Menu {
            monitor,
            _subscriptions: vec![closed],
        }
    }
}

fn menu_item(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(MENU_ITEM_H))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(13.))
        .text_color(ink())
        .hover(|s| s.bg(track()))
        .child(label)
}

impl Render for Menu {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let monitor = self.monitor.clone();
        div().size_full().p(px(8.)).font_family(FONT).child(
            div()
                .size_full()
                .p(px(4.))
                .flex()
                .flex_col()
                .bg(card())
                .border_1()
                .border_color(line())
                .rounded(px(10.))
                .shadow_lg()
                .child(
                    menu_item("clear", "清除已完成").on_click(move |_, window, cx| {
                        monitor.update(cx, |m, cx| {
                            m.clear_finished();
                            cx.notify();
                        });
                        window.remove_window();
                    }),
                )
                .child(menu_item("quit", "退出").on_click(|_, _, cx| cx.quit())),
        )
    }
}
