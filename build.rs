// Windows：把 branding/app-icon.ico 嵌入 exe（资源管理器、任务管理器、Alt+Tab 图标）。
fn main() {
    println!("cargo:rerun-if-changed=branding/app-icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("branding/app-icon.ico");
        resource.compile().expect("embed app icon");
    }
}
