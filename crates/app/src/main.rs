#![cfg_attr(
    all(windows, feature = "gtk-ui", not(debug_assertions)),
    windows_subsystem = "windows"
)]

#[cfg(feature = "gtk-ui")]
mod gtk_app;

#[cfg(all(feature = "gtk-ui", feature = "high-performance-gpu", windows))]
#[used]
#[unsafe(no_mangle)]
pub static NvOptimusEnablement: u32 = 1;

#[cfg(all(feature = "gtk-ui", feature = "high-performance-gpu", windows))]
#[used]
#[unsafe(no_mangle)]
pub static AmdPowerXpressRequestHighPerformance: u32 = 1;

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
        "R-shell built without GTK4. Run `cargo run -p shell-app --features gtk-ui` after installing GTK4 development libraries."
    );
}

#[cfg(all(feature = "gtk-ui", windows))]
fn configure_windows_gtk_runtime() {
    let renderer_backend = load_renderer_backend_setting();

    unsafe {
        match renderer_backend {
            RendererBackend::Cairo => {
                std::env::set_var("GSK_RENDERER", "cairo");
                std::env::remove_var("GDK_WIN32_FORCE_DCOMP");
                set_env_csv_without(
                    "GDK_DISABLE",
                    &["all", "gl", "vulkan", "d3d11", "d3d12", "dcomp"],
                );
                append_env_csv("GDK_DISABLE", &["gl", "vulkan", "d3d11", "d3d12"]);
            }
            RendererBackend::Gl => {
                std::env::set_var("GSK_RENDERER", "gl");
                std::env::set_var("GDK_WIN32_FORCE_DCOMP", "1");
                set_env_csv_without(
                    "GDK_DISABLE",
                    &["all", "gl", "gl-api", "gles-api", "wgl", "dcomp", "d3d11"],
                );
                append_env_csv("GDK_DISABLE", &["d3d12", "vulkan"]);
            }
        }

        std::env::set_var("GIO_USE_VFS", "local");
        std::env::set_var("GSETTINGS_BACKEND", "memory");
    }

    let Ok(exe_path) = std::env::current_exe() else {
        return;
    };
    let Some(bin_dir) = exe_path.parent() else {
        return;
    };
    let prefix_dir = if bin_dir.join("share").exists() {
        bin_dir
    } else if bin_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        bin_dir.parent().unwrap_or(bin_dir)
    } else {
        bin_dir
    };

    let share_dir = prefix_dir.join("share");
    if !share_dir.exists() {
        return;
    }

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

    let fonts_conf = prefix_dir.join("etc").join("fonts").join("fonts.conf");
    if fonts_conf.exists() {
        set_env_path("FONTCONFIG_FILE", &fonts_conf);
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
    let mut values = env_csv_values(name);

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

#[cfg(all(feature = "gtk-ui", windows))]
fn set_env_csv_without(name: &str, removed_values: &[&str]) {
    let values = env_csv_values(name)
        .into_iter()
        .filter(|value| {
            !removed_values
                .iter()
                .any(|removed| value.eq_ignore_ascii_case(removed))
        })
        .collect::<Vec<_>>();

    unsafe {
        if values.is_empty() {
            std::env::remove_var(name);
        } else {
            std::env::set_var(name, values.join(","));
        }
    }
}

#[cfg(all(feature = "gtk-ui", windows))]
fn env_csv_values(name: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_default()
        .split(|ch| matches!(ch, ':' | ';' | ',' | ' ' | '\t'))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(all(feature = "gtk-ui", not(windows)))]
fn configure_windows_gtk_runtime() {}
