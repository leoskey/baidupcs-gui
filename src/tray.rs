//! 系统托盘图标（仅 Windows）：左键单击显示／隐藏胶囊；右键菜单：显示／隐藏胶囊、退出。

use std::time::Duration;

use gpui::{App, WindowHandle};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::ui::Capsule;

pub fn run(capsule: WindowHandle<Capsule>, cx: &mut App) {
    let toggle = MenuItem::new("显示／隐藏胶囊", true, None);
    let quit = MenuItem::new("退出", true, None);
    let menu = Menu::new();
    if menu.append_items(&[&toggle, &quit]).is_err() {
        return;
    }
    // 资源 1 是 build.rs 嵌入 exe 的应用图标
    let Ok(icon) = Icon::from_resource(1, None) else {
        return;
    };
    let Ok(tray) = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("BaiduPCS GUI")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
    else {
        return;
    };
    let (toggle, quit) = (toggle.id().clone(), quit.id().clone());
    // ponytail: 托盘事件进的是同步 channel，这里 100ms 轮询一次；要零延迟再改成 set_event_handler + 异步 channel
    cx.spawn(async move |cx| {
        let _tray = tray; // 托盘图标随任务存活，析构即从托盘移除
        loop {
            let mut flip = false;
            while let Ok(e) = TrayIconEvent::receiver().try_recv() {
                flip ^= matches!(
                    e,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                );
            }
            while let Ok(e) = MenuEvent::receiver().try_recv() {
                if e.id == quit {
                    cx.update(|cx| cx.quit()).ok();
                    return;
                }
                flip ^= e.id == toggle;
            }
            if flip && capsule.update(cx, |c, _, cx| c.toggle_visible(cx)).is_err() {
                return;
            }
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
        }
    })
    .detach();
}
