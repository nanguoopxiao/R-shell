#![cfg_attr(
    all(windows, feature = "gtk-ui", not(debug_assertions)),
    windows_subsystem = "windows"
)]

// 即使不启用 GTK，本二进制也应保持可构建：CI 和核心逻辑开发不应依赖
// 系统 GTK4 开发库。真正的图形界面通过 `gtk-ui` feature 显式启用。

#[cfg(feature = "gtk-ui")]
mod gtk_app;

#[cfg(all(feature = "gtk-ui", windows))]
use shell_storage::{RendererBackend, load_settings};

#[cfg(feature = "gtk-ui")]
fn main() -> anyhow::Result<()> {
    configure_windows_gtk_runtime();
    gtk_app::run()
}

#[cfg(not(feature = "gtk-ui"))]
fn main() {
    println!(
        "shell-app built without GTK4. Run `cargo run -p shell-app --features gtk-ui` after installing GTK4 development libraries."
    );
}

#[cfg(all(feature = "gtk-ui", windows))]
fn configure_windows_gtk_runtime() {
    // 强制 GTK 使用 CPU 路径。MSYS2 GTK4 运行时即使只显示终端界面，也可能
    // 初始化 D3D、Vulkan 和 GStreamer 媒体栈；让 GDK 加载这些后端会在部分
    // Windows 虚拟机或特定显卡驱动上额外占用 150MB 以上私有内存。终端渲染
    // 直接使用 Cairo/Pango，因此不需要这些 GPU 路径。
    //
    // 必须在 g_application_run() 初始化 GDK/GSK 之前设置。
    let renderer_backend = load_renderer_backend_setting();

    unsafe {
        match renderer_backend {
            RendererBackend::Cairo => {
                std::env::set_var("GSK_RENDERER", "cairo");
                append_env_csv("GDK_DISABLE", &["gl", "vulkan", "d3d11", "d3d12"]);
            }
            RendererBackend::Ngl => {
                std::env::set_var("GSK_RENDERER", "ngl");
                append_env_csv("GDK_DISABLE", &["vulkan"]);
            }
        }
        std::env::set_var("GIO_USE_VFS", "local");
        std::env::set_var("GSETTINGS_BACKEND", "memory");
        // 不再设置 GDK_DEBUG=no-offload：GTK4 0.10.x 不识别该调试标志，
        // 会向 stderr 打印警告。
    }

    let Ok(exe_path) = std::env::current_exe() else {
        return;
    };
    let Some(bin_dir) = exe_path.parent() else {
        return;
    };
    let Some(prefix_dir) = bin_dir.parent() else {
        return;
    };

    let share_dir = prefix_dir.join("share");
    if !share_dir.exists() {
        return;
    }

    // 从 `dist/windows-gtk/bin` 启动时，将 GLib/GDK/Pango 指向随包携带的
    // 运行时目录。这样可以保持包的可移植性，并避免误加载用户全局 PATH 中
    // 不兼容的 DLL。
    prepend_env_path("PATH", bin_dir);
    set_env_path("XDG_DATA_DIRS", &share_dir);

    let schema_dir = share_dir.join("glib-2.0").join("schemas");
    if schema_dir.exists() {
        set_env_path("GSETTINGS_SCHEMA_DIR", &schema_dir);
    }

    let pixbuf_dir = prefix_dir
        .join("lib")
        .join("gdk-pixbuf-2.0")
        .join("2.10.0")
        .join("loaders");
    if pixbuf_dir.exists() {
        set_env_path("GDK_PIXBUF_MODULEDIR", &pixbuf_dir);
    }

    let gio_modules_dir = prefix_dir.join("lib").join("gio").join("modules");
    if gio_modules_dir.exists() {
        set_env_path("GIO_MODULE_DIR", &gio_modules_dir);
    }

    // 将 fontconfig 限制到随包携带的最小 fonts.conf，避免 Pango/FreeType 扫描
    // 整个 C:\Windows\Fonts。中文 Windows 中常见 50-100 个 CJK 字体，
    // SimSun-ExtB 单个字体就约 32MB；仅通过 fontconfig+HarfBuzz 加载元数据
    // 也可能带来约 100MB 的内存占用。
    //
    // 当前 fonts.conf 只白名单终端实际需要的字体：一个等宽字体 + 一个中文
    // CJK 字体。
    let fonts_conf = prefix_dir.join("etc").join("fonts").join("fonts.conf");
    if fonts_conf.exists() {
        set_env_path("FONTCONFIG_FILE", &fonts_conf);
        // 将本程序使用的 fontconfig 缓存写到配置旁边，便于多次启动复用，
        // 避免昂贵的初次扫描反复发生。
        let fc_cache_dir = prefix_dir.join("var").join("cache").join("fontconfig");
        if !fc_cache_dir.exists() {
            let _ = std::fs::create_dir_all(&fc_cache_dir);
        }
        if fc_cache_dir.exists() {
            set_env_path("FONTCONFIG_CACHE", &fc_cache_dir);
        }
    }
}

#[cfg(all(feature = "gtk-ui", windows))]
fn load_renderer_backend_setting() -> RendererBackend {
    let settings_path = dirs_next::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("shell")
        .join("settings.json");

    load_settings(&settings_path)
        .map(|settings| settings.renderer_backend)
        .unwrap_or_default()
}

#[cfg(all(feature = "gtk-ui", windows))]
fn prepend_env_path(name: &str, path: &std::path::Path) {
    let mut paths = vec![path.to_path_buf()];
    if let Some(current) = std::env::var_os(name) {
        paths.extend(std::env::split_paths(&current));
    }

    if let Ok(joined) = std::env::join_paths(paths) {
        unsafe {
            std::env::set_var(name, joined);
        }
    }
}

#[cfg(all(feature = "gtk-ui", windows))]
fn set_env_path(name: &str, value: &std::path::Path) {
    unsafe {
        std::env::set_var(name, value);
    }
}

#[cfg(all(feature = "gtk-ui", windows))]
fn append_env_csv(name: &str, required_values: &[&str]) {
    let current = std::env::var(name).unwrap_or_default();
    let mut values = current
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    for required in required_values {
        if !values
            .iter()
            .any(|value| value.eq_ignore_ascii_case(required))
        {
            values.push((*required).to_string());
        }
    }

    unsafe {
        std::env::set_var(name, values.join(","));
    }
}

#[cfg(all(feature = "gtk-ui", not(windows)))]
fn configure_windows_gtk_runtime() {}
