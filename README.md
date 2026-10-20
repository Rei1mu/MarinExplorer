# MarinExplorer

一个轻量、支持 Windows on ARM64、基于 Rust + egui 的双栏文件管理器 / A lightweight, Windows-on-ARM64-capable, Rust + egui based two-pane file manager.

- 中文文档 ↓ [中文文档](#中文文档)
- English documentation ↓ [English Documentation](#english-documentation)

---

## 中文文档

一个**轻量、支持 Windows on ARM64、基于 Rust + [egui](https://github.com/emilk/egui) 的双栏文件管理器**。

**仅支持 Windows**——`x86_64`（标准 Windows 64 位）与 `aarch64`（Windows on ARM64，如 Surface / 骁龙笔记本）。不提供 Linux 或 macOS 版本。

### 运行

直接启动 `MarinExplorer.exe`。首次打开会读取并缓存磁盘信息，之后都很快。

💡 **建议以管理员身份运行**：访问受保护的系统目录、写入系统路径时，管理员权限能避免因权限不足导致的功能受限。

🔒 **安全提醒**：请只从本仓库的 **Releases** 或官方构建渠道获取 `MarinExplorer.exe`。不要从不明来源下载，以免被植入木马或后门。

### 它解决了什么

1. **资源占用太大** — 资源管理器偏重。MarinExplorer 用 Rust + egui 自绘 UI，常驻内存远低于系统资源管理器，在内存吃紧的 ARM64 设备上差别尤其明显。
2. **回收站卡死** — 回收站里堆到数万文件时，资源管理器要花特别久才能读出列表，删除更是近乎不可用。这里把 `$Recycle.Bin` 里 `$I` 文件的解析丢到**后台线程**，界面不等它、全程可操作；清空后自动刷新，侧边栏图标随「满 / 空」变化。
3. **图片预览各自为政** — 每台设备装了不同的看图软件，双击一张 PNG 会拉起不同的程序，各自再占一份内存。内置预览面板在同一个窗口里直接看图，不必再启动别的应用。
4. **图片转 ICO 麻烦** — 开发软件要给应用做图标时，往往得去找在线转换站或额外装工具。在预览器里右键「转换为 ICO」，直接把当前图片（可拖拽框选方形裁切区域）导出成**一个**多尺寸 `.ico`（16 / 24 / 32 / 48 / 64 / 128 / 256 任选），落回原图所在目录。
5. **图片压缩 / 转 PNG** — 右键「转换为 PNG」，可选 0–9 的压缩级别（0 = 不压缩，9 = 最小），导出到新文件夹，避免同名覆盖源文件。

当前形态：一个基于 Rust + egui 的 Windows 桌面文件管理器，专注「能用、顺手」而非功能堆砌，原生支持 Windows on ARM64（msvc）。

### 功能

**双栏浏览**
- 左右两个**互相独立**的文件面板，各自维护：当前目录、筛选、排序、历史。
- 顶部工具栏：`返回 / 前进 / 上级 / 刷新 / 复制 / 移动 / 粘贴 / 删除 / 新建文件夹 / 跳转 / 终端 / 显示隐藏文件`。

**侧边栏**
- 回收站入口（列表后台异步加载，不卡 UI；清空后自动刷新，图标随满/空状态变化）。
- 固定磁盘列表，带**剩余 / 总量**空间显示。
- 主页（Home）快捷入口。

**预览与图片工具**
- 内置图片预览面板，支持 PNG / JPEG / BMP / ICO / TIFF / WebP / GIF。
- 缩略图网格视图。
- **转换为 ICO**：拖拽框选方形区域，勾选需要的尺寸（16 / 24 / 32 / 48 / 64 / 128 / 256），导出**单个**多尺寸 `.ico` 到原图所在目录（Vista+ 格式，内嵌 PNG 载荷）。
- **转换为 PNG**：可选 0–9 压缩级别（0 = 不压缩，9 = 最小），导出到新文件夹，不会覆盖同名源文件。

**拖拽与外壳集成**
- 从面板**拖出文件**到外部程序。
- 使用系统**真实文件图标**（shell icon），而不是自己画一套。
- 支持 Windows 原生：深色标题栏、应用图标、系统托盘常驻。

**性能与体验**
- 文件夹大小在**后台线程**计算，并持久化到磁盘（`%APPDATA%\rust-explorer\store.txt`），重开即秒显。
- 底部状态栏：状态、当前路径、条目数、任务进度。
- 多语言界面：**简体中文 / English / 日本語 / 繁體中文**。
- 亮色 / 暗色主题；中文用 **Windows 系统字体**（微软雅黑优先，运行时从 `%WINDIR%\Fonts` 加载，不打包进 exe），观感与系统一致且不乱码。

**键盘**
`↑/↓` 选择 · `Enter` 打开 · `Home/End` 跳首尾 · `Delete` 删除 · `Alt+←/→` 后退/前进 · `Esc` 关闭弹窗。

### 构建

需要 Rust 工具链。本项目只产出 **Windows** 可执行文件，三个目标：

- `x86_64-pc-windows-gnu` —— 标准 Windows x64，使用 MinGW-w64（GCC）。
- `x86_64-pc-windows-msvc` —— 标准 Windows x64，使用 MSVC。
- `aarch64-pc-windows-msvc` —— Windows on ARM64（如 Surface / 骁龙笔记本），使用 MSVC。

```bash
# 默认目标
cargo build --release

# Windows x64（gnu）
cargo build --release --target x86_64-pc-windows-gnu

# Windows x64（msvc）
cargo build --release --target x86_64-pc-windows-msvc

# Windows on ARM64（msvc）
cargo build --release --target aarch64-pc-windows-msvc
```

产物：
- `target/x86_64-pc-windows-gnu/release/MarinExplorer.exe`
- `target/x86_64-pc-windows-msvc/release/MarinExplorer.exe`
- `target/aarch64-pc-windows-msvc/release/MarinExplorer.exe`

**GNU 版和 MSVC 版有什么区别？**

两个 x64 版本能在同一台普通 64 位 Windows 上跑，功能完全一致，区别只在"怎么编出来的"：

| | GNU（MinGW-w64） | MSVC |
|---|---|---|
| C 运行时 | MinGW 自带（libgcc / winpthread + 系统 msvcrt） | 微软运行时，CI 已静态链接（`-C target-feature=+crt-static`） |
| 要装 VC++ 运行库吗 | 不需要 | 不需要（已静态链接进去） |
| exe 文件图标 | 有（`build.rs` 用 windres 内嵌） | 有（`build.rs` 用 rc.exe 内嵌） |
| 调试生态 | GCC / DWARF | Visual Studio / WinDbg / PDB |
| 与 C 库互操作 | 只能链 MinGW 的 `.a` | 只能链 MSVC 的 `.lib` |

说明：
- 发布构建已为体积优化（`opt-level = "z"` + LTO + strip）。
- 应用图标由 `build.rs` 编进 Windows 资源，三个产物都带：**GNU 版用 MinGW 的 `windres` 生成 `.o`，MSVC 版用 Windows SDK 的 `rc.exe` 生成 `.res`**。找不到资源编译器时编译照常通过，只是没有文件图标。
- 代码签名（可选）见 `sign.ps1`：用受信任 CA 的 `.pfx` 给 exe 签名 + 时间戳，消除 SmartScreen 拦截。
- 普通用户直接用 **GNU 版**即可（有文件图标、无运行时依赖）。只有要接入 MSVC 编译的库、或要用 VS / WinDbg 调试时，才必须选 MSVC 版。

### 自动构建（GitHub Actions）

仓库已在每次推送 `main` 时通过 GitHub Actions 自动编译 **三个 Windows 版本**（GNU x64 在 Linux 上用 MinGW-w64（GCC）交叉编译；两个 MSVC 版本在 Windows runner 上用原生 MSVC 工具链编译，产物全是 Windows `.exe`）：

- `x86_64-pc-windows-gnu`（标准 Windows x64，GNU）
- `x86_64-pc-windows-msvc`（标准 Windows x64，MSVC）
- `aarch64-pc-windows-msvc`（Windows on ARM64，MSVC）

工作流见 [`.github/workflows/build.yml`](.github/workflows/build.yml)。三个产物名为 `MarinExplorer-x64-gnu.exe`、`MarinExplorer-x64-msvc.exe` 与 `MarinExplorer-arm64.exe`，可在 **Actions → 对应运行 → Artifacts** 下载；打 `v*` 标签推送时会自动发布到 **Releases**。

### 许可证

[MIT](./LICENSE) © 2026 Rei1mu

自由使用、修改、再分发，只需保留版权声明。

---

## English Documentation

A **lightweight, Windows-on-ARM64-capable, Rust + [egui](https://github.com/emilk/egui) based two-pane file manager**.

**Windows only** — `x86_64` (standard 64-bit Windows) and `aarch64` (Windows on ARM64, e.g. Surface / Snapdragon laptops). No Linux or macOS builds.

### Usage

Just launch `MarinExplorer.exe`. The first open reads and caches disk info; afterwards it's fast.

💡 **Run as Administrator** is recommended: it avoids permission errors when reaching protected system folders or writing to system paths.

🔒 **Security**: only download `MarinExplorer.exe` from this repo's **Releases** or official build artifacts. Do not fetch it from unknown sources — that's how trojans and backdoors get in.

### What It Fixes

1. **Too much resource usage** — Explorer is heavy. MarinExplorer draws its own UI with Rust + egui and stays resident for far less memory than Explorer, which is especially noticeable on memory-tight ARM64 devices.
2. **A frozen Recycle Bin** — once the bin holds tens of thousands of entries, Explorer takes ages just to list it, and deleting is close to unusable. Here the `$I` file parsing inside `$Recycle.Bin` runs on a **background thread**: the UI never waits for it and stays usable the whole time. Emptying auto-refreshes the listing, and the sidebar icon tracks full/empty.
3. **Every machine previews images differently** — each device has its own photo viewer installed, so double-clicking a PNG launches a different program each time, each costing another slice of memory. The built-in preview panel shows images inside the same window; nothing else needs to start.
4. **Image-to-ICO is a hassle** — shipping an app means shipping an icon, which usually means hunting for an online converter or installing extra tooling. Right-click in the viewer → "Convert to ICO" exports the current picture (optionally cropped to a dragged square region) as **one** multi-size `.ico` (pick from 16 / 24 / 32 / 48 / 64 / 128 / 256), written next to the source image.
5. **Image compression / PNG export** — "Convert to PNG" offers deflate levels 0–9 (0 = none, 9 = smallest) and writes into a new folder, so it can never overwrite the same-named source.

Current form: a Rust + egui Windows desktop file manager focused on "works and feels right" rather than feature count, with native support for Windows on ARM64 (MSVC).

### Features

**Two-pane browsing**
- Two **independent** panels, each keeping its own directory, filter, sort order and history.
- Toolbar: back / forward / up / refresh / copy / move / paste / delete / new folder / jump / terminal / show hidden.

**Sidebar**
- Recycle Bin entry (loaded async in the background; auto-refreshes after emptying, icon reflects full/empty).
- Fixed drive list with **free / total** space.
- Home shortcut.

**Preview & image tools**
- Built-in image preview: PNG / JPEG / BMP / ICO / TIFF / WebP / GIF.
- Thumbnail grid view.
- **Convert to ICO**: drag a square crop region, tick the sizes you need (16 / 24 / 32 / 48 / 64 / 128 / 256), and export a **single** multi-size `.ico` next to the source (Vista+ format, PNG payloads inside).
- **Convert to PNG**: choose a deflate level 0–9 (0 = none, 9 = smallest); output goes into a new folder so the same-named source is never overwritten.

**Drag & shell integration**
- Drag files **out** of a panel into other apps.
- Uses the system's **real file icons** (shell icons), not a hand-drawn set.
- Native Windows: dark title bar, app icon, system tray resident.

**Performance & UX**
- Folder sizes computed in a **background thread** and persisted (`%APPDATA%\rust-explorer\store.txt`) for instant reload.
- Status bar: state, current path, item count, task progress.
- UI languages: Simplified Chinese / English / Japanese / Traditional Chinese.
- Light / dark theme; Chinese uses the **Windows system font** (Microsoft YaHei first, loaded at runtime from `%WINDIR%\Fonts` — not bundled), so it matches the OS look and never mojibakes.

**Keyboard**
`↑/↓` select · `Enter` open · `Home/End` jump to top/bottom · `Delete` delete · `Alt+←/→` back/forward · `Esc` close dialog.

### Build

Requires the Rust toolchain. This project builds **Windows** executables only, for three targets:

- `x86_64-pc-windows-gnu` — standard Windows x64, built with MinGW-w64 (GCC).
- `x86_64-pc-windows-msvc` — standard Windows x64, built with MSVC.
- `aarch64-pc-windows-msvc` — Windows on ARM64 (e.g. Surface / Snapdragon laptops), built with MSVC.

```bash
# default target
cargo build --release

# Windows x64 (gnu)
cargo build --release --target x86_64-pc-windows-gnu

# Windows x64 (msvc)
cargo build --release --target x86_64-pc-windows-msvc

# Windows on ARM64 (msvc)
cargo build --release --target aarch64-pc-windows-msvc
```

Artifacts:
- `target/x86_64-pc-windows-gnu/release/MarinExplorer.exe`
- `target/x86_64-pc-windows-msvc/release/MarinExplorer.exe`
- `target/aarch64-pc-windows-msvc/release/MarinExplorer.exe`

**How do the GNU and MSVC builds differ?**

Both x64 editions run on the same ordinary 64-bit Windows PC and are functionally identical — they differ only in how they were compiled:

| | GNU (MinGW-w64) | MSVC |
|---|---|---|
| C runtime | MinGW's own (libgcc / winpthread + system msvcrt) | Microsoft's, linked **statically** in CI (`-C target-feature=+crt-static`) |
| Needs VC++ redistributable | No | No (statically linked in) |
| exe file icon | Yes (embedded by `build.rs` via `windres`) | Yes (embedded by `build.rs` via `rc.exe`) |
| Debug ecosystem | GCC / DWARF | Visual Studio / WinDbg / PDB |
| C interop | Links MinGW `.a` only | Links MSVC `.lib` only |

Notes:
- Release builds are size-optimized (`opt-level = "z"` + LTO + strip).
- The app icon is embedded by `build.rs` in all three builds: MinGW's `windres` for the GNU target, Windows SDK `rc.exe` for the MSVC targets. If no resource compiler is found the build still succeeds, just without a file icon.
- Optional code signing lives in `sign.ps1`.
- For everyday use just take the **GNU build** (file icon, no runtime dependency). You only need the MSVC build to link MSVC-compiled libraries, or to debug with VS / WinDbg.

### Automated Builds (GitHub Actions)

On every push to `main`, GitHub Actions automatically builds **three Windows editions** (GNU x64 is cross-compiled on Linux with MinGW-w64 (GCC); both MSVC editions are built on a Windows runner with the native MSVC toolchain — every artifact is a Windows `.exe`):

- `x86_64-pc-windows-gnu` (standard Windows x64, GNU)
- `x86_64-pc-windows-msvc` (standard Windows x64, MSVC)
- `aarch64-pc-windows-msvc` (Windows on ARM64, MSVC)

Workflow: [`.github/workflows/build.yml`](.github/workflows/build.yml). The three artifacts are `MarinExplorer-x64-gnu.exe`, `MarinExplorer-x64-msvc.exe` and `MarinExplorer-arm64.exe`, downloadable from **Actions → run → Artifacts**; pushing a `v*` tag also publishes them to **Releases**.

### License

[MIT](./LICENSE) © 2026 Rei1mu

Free to use, modify, and redistribute — just keep the copyright notice.
