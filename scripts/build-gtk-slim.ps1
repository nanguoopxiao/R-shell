param(
    [string]$GtkVersion = '4.22.4',
    [string]$BuildRoot = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')) 'build\gtk-slim'),
    [switch]$InstallBuildTools,
    [switch]$Package,
    [string]$PackageDestination = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')) 'dist\windows-gtk-slim')
)

$ErrorActionPreference = 'Stop'

$Root = Resolve-Path (Join-Path $PSScriptRoot '..')
$MsysRoot = if ($env:MSYS2_ROOT) {
    $env:MSYS2_ROOT
} elseif ($env:MSYS2_LOCATION) {
    $env:MSYS2_LOCATION
} else {
    Join-Path $Root '.msys64'
}

$BashPath = Join-Path $MsysRoot 'usr\bin\bash.exe'
$MingwBin = Join-Path $MsysRoot 'mingw64\bin'
$UsrBin = Join-Path $MsysRoot 'usr\bin'

if (-not (Test-Path $BashPath)) {
    throw "bash.exe was not found under $MsysRoot. Install or copy MSYS2 to the repo-local .msys64 directory first."
}

function Convert-ToMsysPath {
    param([string]$Path)

    $fullPath = [IO.Path]::GetFullPath($Path)
    if ($fullPath -notmatch '^([A-Za-z]):\\(.*)$') {
        throw "Cannot convert path to MSYS form: $fullPath"
    }

    $drive = $matches[1].ToLowerInvariant()
    $rest = $matches[2] -replace '\\', '/'
    "/$drive/$rest"
}

function Invoke-MsysBash {
    param([string]$Script)

    & $BashPath -lc $Script
    if ($LASTEXITCODE -ne 0) {
        throw "MSYS2 command failed with exit code $LASTEXITCODE."
    }
}

if ($InstallBuildTools) {
    Invoke-MsysBash @'
set -euo pipefail
pacman -S --needed --noconfirm \
  base-devel git patch \
  mingw-w64-x86_64-meson \
  mingw-w64-x86_64-ninja \
  mingw-w64-x86_64-pkgconf \
  mingw-w64-x86_64-gobject-introspection \
  mingw-w64-x86_64-sassc \
  mingw-w64-x86_64-python-docutils \
  mingw-w64-x86_64-gi-docgen
'@
}

$RequiredTools = @(
    (Join-Path $MingwBin 'meson.exe'),
    (Join-Path $MingwBin 'ninja.exe'),
    (Join-Path $MingwBin 'pkg-config.exe'),
    (Join-Path $MingwBin 'objdump.exe'),
    (Join-Path $UsrBin 'patch.exe'),
    (Join-Path $UsrBin 'tar.exe'),
    (Join-Path $UsrBin 'curl.exe')
)

$MissingTools = $RequiredTools | Where-Object { -not (Test-Path $_) }
if ($MissingTools) {
    $list = ($MissingTools | ForEach-Object { "  $_" }) -join [Environment]::NewLine
    throw "GTK slim build tools are missing. Run this script again with -InstallBuildTools.`n$list"
}

New-Item -ItemType Directory -Path $BuildRoot -Force | Out-Null

$FontPatch = @'
diff --git a/gtk/gtksettings.c b/gtk/gtksettings.c
--- a/gtk/gtksettings.c
+++ b/gtk/gtksettings.c
@@ -984,4 +984,4 @@ gtk_settings_class_init (GtkSettingsClass *class)
   pspecs[PROP_FONT_RENDERING] = g_param_spec_enum ("gtk-font-rendering", NULL, NULL,
                                                    GTK_TYPE_FONT_RENDERING,
-                                                   GTK_FONT_RENDERING_AUTOMATIC,
+                                                   GTK_FONT_RENDERING_MANUAL,
                                                    GTK_PARAM_READWRITE);
'@

$DcompPatch = @'
diff --git a/gdk/win32/gdkdisplay-win32.c b/gdk/win32/gdkdisplay-win32.c
--- a/gdk/win32/gdkdisplay-win32.c
+++ b/gdk/win32/gdkdisplay-win32.c
@@ -520 +520 @@ gdk_win32_display_init_dcomp (GdkWin32Display *self)
-  if (!gdk_has_feature (GDK_FEATURE_DCOMP))
+  if (!gdk_has_feature (GDK_FEATURE_DCOMP) || g_getenv ("GDK_WIN32_FORCE_DCOMP") == NULL)
'@

$LazyD3D12Patch = @'
diff --git a/gdk/win32/meson.build b/gdk/win32/meson.build
--- a/gdk/win32/meson.build
+++ b/gdk/win32/meson.build
@@ -61,6 +61,5 @@
 d3d12_dep = [
   cc.find_library('d3d11'),
-  cc.find_library('d3d12'),
   cc.find_library('dcomp'),
   cc.find_library('dxgi'),
   cc.find_library('dxguid'),
diff --git a/gdk/win32/gdkdisplay-win32.c b/gdk/win32/gdkdisplay-win32.c
--- a/gdk/win32/gdkdisplay-win32.c
+++ b/gdk/win32/gdkdisplay-win32.c
@@ -46,5 +46,33 @@
 #ifndef IMAGE_FILE_MACHINE_ARM64
 # define IMAGE_FILE_MACHINE_ARM64 0xAA64
 #endif
 
+typedef HRESULT (WINAPI *GdkWin32D3D12CreateDeviceFunc) (IUnknown          *adapter,
+                                                         D3D_FEATURE_LEVEL  minimum_feature_level,
+                                                         REFIID             riid,
+                                                         void             **device);
+
+static HRESULT
+gdk_win32_d3d12_create_device (IUnknown          *adapter,
+                               D3D_FEATURE_LEVEL  minimum_feature_level,
+                               REFIID             riid,
+                               void             **device)
+{
+  static HMODULE d3d12_module;
+  static GdkWin32D3D12CreateDeviceFunc create_device;
+
+  if (create_device == NULL)
+    {
+      d3d12_module = LoadLibraryW (L"d3d12.dll");
+      if (d3d12_module == NULL)
+        return HRESULT_FROM_WIN32 (GetLastError ());
+
+      create_device = (GdkWin32D3D12CreateDeviceFunc) GetProcAddress (d3d12_module, "D3D12CreateDevice");
+      if (create_device == NULL)
+        return HRESULT_FROM_WIN32 (GetLastError ());
+    }
+
+  return create_device (adapter, minimum_feature_level, riid, device);
+}
+
 /**
  * gdk_win32_display_add_filter:
@@ -490,6 +517,6 @@ gdk_win32_display_create_d3d_devices (GdkWin32Display  *self,
       if (d3d12_device != NULL)
         {
-          hr = D3D12CreateDevice ((IUnknown *) adapter,
-                                  D3D_FEATURE_LEVEL_12_0,
-                                  &IID_ID3D12Device,
-                                  (void **) d3d12_device);
+          hr = gdk_win32_d3d12_create_device ((IUnknown *) adapter,
+                                              D3D_FEATURE_LEVEL_12_0,
+                                              &IID_ID3D12Device,
+                                              (void **) d3d12_device);
diff --git a/gdk/win32/gdkd3d12utils.c b/gdk/win32/gdkd3d12utils.c
--- a/gdk/win32/gdkd3d12utils.c
+++ b/gdk/win32/gdkd3d12utils.c
@@ -23,4 +23,32 @@
 #include "gdkmemoryformatprivate.h"
 #include "gdkprivate-win32.h"
 
+typedef HRESULT (WINAPI *GdkWin32D3D12CreateDeviceFunc) (IUnknown          *adapter,
+                                                         D3D_FEATURE_LEVEL  minimum_feature_level,
+                                                         REFIID             riid,
+                                                         void             **device);
+
+static HRESULT
+gdk_win32_d3d12_create_device (IUnknown          *adapter,
+                               D3D_FEATURE_LEVEL  minimum_feature_level,
+                               REFIID             riid,
+                               void             **device)
+{
+  static HMODULE d3d12_module;
+  static GdkWin32D3D12CreateDeviceFunc create_device;
+
+  if (create_device == NULL)
+    {
+      d3d12_module = LoadLibraryW (L"d3d12.dll");
+      if (d3d12_module == NULL)
+        return HRESULT_FROM_WIN32 (GetLastError ());
+
+      create_device = (GdkWin32D3D12CreateDeviceFunc) GetProcAddress (d3d12_module, "D3D12CreateDevice");
+      if (create_device == NULL)
+        return HRESULT_FROM_WIN32 (GetLastError ());
+    }
+
+  return create_device (adapter, minimum_feature_level, riid, device);
+}
+
 /*<private>
  * gdk_d3d12_resource_get_layout:
@@ -86,5 +113,5 @@ gdk_d3d12_resource_new_from_bytes (const guchar           *data,
   DXGI_FORMAT format;
   void *buffer_data;
   gsize p;
 
-  hr = D3D12CreateDevice (NULL, D3D_FEATURE_LEVEL_12_0, &IID_ID3D12Device, (void **) &device);
+  hr = gdk_win32_d3d12_create_device (NULL, D3D_FEATURE_LEVEL_12_0, &IID_ID3D12Device, (void **) &device);
'@

Set-Content -Path (Join-Path $BuildRoot '001-fix-font-rendering.patch') -Value $FontPatch -Encoding ascii
Set-Content -Path (Join-Path $BuildRoot '003-default-dcomp-off.patch') -Value $DcompPatch -Encoding ascii

$LazyD3D12Python = @'
from pathlib import Path

def replace_once(path, old, new):
    text = path.read_text()
    if old not in text:
        raise SystemExit(f"pattern not found in {path}: {old!r}")
    path.write_text(text.replace(old, new, 1))

helper = """typedef HRESULT (WINAPI *GdkWin32D3D12CreateDeviceFunc) (IUnknown          *adapter,
                                                         D3D_FEATURE_LEVEL  minimum_feature_level,
                                                         REFIID             riid,
                                                         void             **device);

static HRESULT
gdk_win32_d3d12_create_device (IUnknown          *adapter,
                               D3D_FEATURE_LEVEL  minimum_feature_level,
                               REFIID             riid,
                               void             **device)
{
  static HMODULE d3d12_module;
  static GdkWin32D3D12CreateDeviceFunc create_device;

  if (create_device == NULL)
    {
      d3d12_module = LoadLibraryW (L"d3d12.dll");
      if (d3d12_module == NULL)
        return HRESULT_FROM_WIN32 (GetLastError ());

      create_device = (GdkWin32D3D12CreateDeviceFunc) GetProcAddress (d3d12_module, "D3D12CreateDevice");
      if (create_device == NULL)
        return HRESULT_FROM_WIN32 (GetLastError ());
    }

  return create_device (adapter, minimum_feature_level, riid, device);
}
"""

replace_once(
    Path("gdk/win32/meson.build"),
    "  cc.find_library('d3d12'),\n",
    "",
)

replace_once(
    Path("gdk/win32/gdkdisplay-win32.c"),
    "#endif\n\n/**\n * gdk_win32_display_add_filter:",
    f"#endif\n\n{helper}\n/**\n * gdk_win32_display_add_filter:",
)
replace_once(
    Path("gdk/win32/gdkdisplay-win32.c"),
    """          hr = D3D12CreateDevice ((IUnknown *) adapter,
                                  D3D_FEATURE_LEVEL_12_0,
                                  &IID_ID3D12Device,
                                  (void **) d3d12_device);""",
    """          hr = gdk_win32_d3d12_create_device ((IUnknown *) adapter,
                                              D3D_FEATURE_LEVEL_12_0,
                                              &IID_ID3D12Device,
                                              (void **) d3d12_device);""",
)

replace_once(
    Path("gdk/win32/gdkd3d12utils.c"),
    '#include "gdkprivate-win32.h"\n\n/*<private>',
    f'#include "gdkprivate-win32.h"\n\n{helper}\n/*<private>',
)
replace_once(
    Path("gdk/win32/gdkd3d12utils.c"),
    "  hr = D3D12CreateDevice (NULL, D3D_FEATURE_LEVEL_12_0, &IID_ID3D12Device, (void **) &device);",
    "  hr = gdk_win32_d3d12_create_device (NULL, D3D_FEATURE_LEVEL_12_0, &IID_ID3D12Device, (void **) &device);",
)
'@
Set-Content -Path (Join-Path $BuildRoot '004-lazy-d3d12-load.py') -Value $LazyD3D12Python -Encoding ascii

$MinorVersion = ($GtkVersion -split '\.')[0..1] -join '.'
$BuildRootMsys = Convert-ToMsysPath $BuildRoot
$SourceArchive = "gtk-$GtkVersion.tar.xz"
$SourceUrl = "https://download.gnome.org/sources/gtk/$MinorVersion/$SourceArchive"

Invoke-MsysBash @"
set -euo pipefail
export PATH=/mingw64/bin:/usr/bin
cd '$BuildRootMsys'
if [ ! -f '$SourceArchive' ]; then
  curl -L '$SourceUrl' -o '$SourceArchive'
fi
rm -rf 'gtk-$GtkVersion' build-slim
tar -xf '$SourceArchive'
cd 'gtk-$GtkVersion'
patch -Np1 -i ../001-fix-font-rendering.patch
patch -Np1 -i ../003-default-dcomp-off.patch
python ../004-lazy-d3d12-load.py
cd ..
MSYS2_ARG_CONV_EXCL='--prefix=' meson setup build-slim 'gtk-$GtkVersion' \
  --prefix='$BuildRootMsys/install/mingw64' \
  --wrap-mode=nodownload \
  --auto-features=disabled \
  --buildtype=release \
  -Dbuild-demos=false \
  -Dbuild-examples=false \
  -Dbuild-tests=false \
  -Dbuild-testsuite=false \
  -Ddocumentation=false \
  -Dman-pages=false \
  -Dscreenshots=false \
  -Dmacos-backend=false \
  -Dandroid-backend=false \
  -Dx11-backend=false \
  -Dwayland-backend=false \
  -Dbroadway-backend=false \
  -Dwin32-backend=true \
  -Dmedia-gstreamer=disabled \
  -Dvulkan=disabled \
  -Dprint-cpdb=disabled \
  -Dprint-cups=disabled \
  -Dcloudproviders=disabled \
  -Dsysprof=disabled \
  -Dtracker=disabled \
  -Dcolord=disabled \
  -Daccesskit=disabled \
  -Dandroid-runtime=disabled \
  -Dintrospection=disabled
meson compile -C build-slim
objdump -p build-slim/gtk/libgtk-4-1.dll | grep -E 'DLL Name: (libgst|d3d12\.dll|vulkan-1\.dll)' && exit 2 || true
"@

$SlimGtkBin = Join-Path $BuildRoot 'build-slim\gtk'
$SlimGtkDll = Join-Path $SlimGtkBin 'libgtk-4-1.dll'
if (-not (Test-Path $SlimGtkDll)) {
    throw "Slim GTK DLL was not produced at $SlimGtkDll."
}

Write-Host "Slim GTK DLL: $SlimGtkDll"
Write-Host "Verified: no direct libgst*, d3d12.dll, or vulkan-1.dll imports."

if ($Package) {
    & (Join-Path $PSScriptRoot 'package-gtk.ps1') -Destination $PackageDestination -GtkRuntimeBin $SlimGtkBin
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}
