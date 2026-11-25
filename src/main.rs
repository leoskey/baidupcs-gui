#![cfg_attr(not(test), windows_subsystem = "windows")]
//! 百度网盘传输监视器：自动发现 BaiduPCS-Go 上传／下载任务，置顶悬浮胶囊显示进度。

mod monitor;
mod platform;
mod store;
mod ui;

use std::time::Duration;

use gpui::{AppContext, Application};

fn main() {
    // 单实例：本机端口被占用说明已有胶囊在运行
    // ponytail: 端口锁足够简单且跨平台；如与其他程序冲突再换成命名互斥量／文件锁
    let Ok(_instance) = std::net::TcpListener::bind(("127.0.0.1", 47613)) else {
        return;
    };

    Application::new().with_assets(ui::Assets).run(|cx| {
        let monitor = cx.new(|_| monitor::Monitor::default());
        let poller = monitor.clone();
        cx.spawn(async move |cx| {
            loop {
                if poller
                    .update(cx, |m, cx| {
                        m.poll();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(1500))
                    .await;
            }
        })
        .detach();
        ui::open_capsule(monitor, cx);
    });
}
