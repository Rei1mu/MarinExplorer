# MarinExplorer

一个轻量、支持 Windows on ARM64、基于 Rust + egui 的双栏文件管理器 / A lightweight, Windows-on-ARM64-capable, Rust + egui based two-pane file manager.

- 中文文档 ↓ [中文文档](#中文文档)
- English documentation ↓ [English Documentation](#english-documentation)

---

## 中文文档

一个**轻量、支持 Windows on ARM64、基于 Rust + [egui](https://github.com/emilk/egui) 的双栏文件管理器**。

**仅支持 Windows**——`x86_64`（标准 Windows 64 位）与 `aarch64`（Windows on ARM64，如 Surface / 骁龙笔记本）。不提供 Linux 或 macOS 版本。

设计目标只有一条：**普通人也能顺手用**。没有华丽的架构，没有学习成本，打开就能用。

### 项目由来（Vibe Coding）

MarinExplorer 是一个典型的 **vibe coding** 产物——它不是一个先画好架构图再填代码的工程，而是在 AI 结对的快速迭代里「长」出来的。

起点只是很朴素的一个念头：系统自带的文件管理器要么太重、要么不顺手，而主流第三方工具又塞了太多用不上的功能。于是就这么一句话开始，在 AI 辅助下一次次打磨：

> 「这个按钮点了没反应」
> 「图标带黑底，alpha 没取对」
> 「回收站一打开就卡死」
> 「这行字太灰，看不清」

代码里保留了大量中文注释，因为作者和 AI 全程用中文交流。功能优先级永远是**易用性 > 精度**：宁可牺牲一点严谨，也要让第一次打开的人不用看说明书。

当前形态：一个基于 Rust + egui 的 Windows 桌面文件管理器，专注「能用、顺手」而非功能堆砌，并原生支持 Windows on ARM64（msvc）。

### 功能

**双栏浏览**
- 左右两个**互相独立**的文件面板，各自维护：当前目录、筛选、排序、历史。
- 顶部工具栏：`返回 / 前进 / 上级 / 刷新 / 复制 / 移动 / 粘贴 / 删除 / 新建文件夹 / 跳转 / 终端 / 显示隐藏文件`。

**侧边栏**
- 回收站入口（列表后台异步加载，不卡 UI；清空后自动刷新，图标随满/空状态变化）。
- 固定磁盘列表，带**剩余 / 总量**空间显示。
- 主页（Home）快捷入口。

**预览与缩略图**
- 内置图片预览面板，支持 PNG / JPEG / BMP / ICO / TIFF / WebP / GIF。
- 缩略图网格视图。
- 「转换为 PNG」对话框（PNG 编码带真实压缩级别）。

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

### 运行

直接启动 `MarinExplorer.exe`。首次打开会读取并缓存磁盘信息，之后都很快。

💡 **建议以管理员身份运行**：访问受保护的系统目录、写入系统路径、以及正常弹出/弹出移动设备时，管理员权限能避免权限不足导致的功能受限。

🔒 **安全提醒**：请只从本仓库的 **Releases** 或官方构建渠道获取 `MarinExplorer.exe`。不要从不明来源下载，以免被植入木马或后门。

### 许可证

[MIT](./LICENSE) © 2026 Rei1mu

自由使用、修改、再分发，只需保留版权声明。

---

## English Documentation

A **lightweight, Windows-on-ARM64-capable, Rust + [egui](https://github.com/emilk/egui) based two-pane file manager**.

**Windows only** — `x86_64` (standard 64-bit Windows) and `aarch64` (Windows on ARM64, e.g. Surface / Snapdragon laptops). No Linux or macOS builds.

The only design goal: **usable by anyone, out of the box**. No fancy architecture, no learning curve — open it and it just works.

### Origin (Vibe Coding)

MarinExplorer is a textbook **vibe coding** artifact — not a project that started from an architecture diagram, but one that *grew* out of rapid, AI-paired iteration.

It started from a simple thought: the built-in file manager is either too heavy or too clunky, while most third-party tools pile on features nobody asked for. From that one line, it was refined again and again with AI assistance:

> "This button does nothing when I click it"
> "The icon has a black background — wrong alpha"
> "The Recycle Bin freezes the moment it opens"
> "This text is too grey, I can't read it"

The code keeps plenty of Chinese comments because the author and the AI talked in Chinese the whole way. Feature priority is always **usability > precision**: we'd rather give up a little rigour than make a first-time user read a manual.

Current form: a Rust + egui Windows desktop file manager focused on "works and feels right" rather than feature count, with native support for Windows on ARM64 (MSVC).

### Features

**Two-pane browsing**
- Two **independent** panels, each keeping its own directory, filter, sort order and history.
- Toolbar: back / forward / up / refresh / copy / move / paste / delete / new folder / jump / terminal / show hidden.

**Sidebar**
- Recycle Bin entry (loaded async in the background; auto-refreshes after emptying, icon reflects full/empty).
- Fixed drive list with **free / total** space.
- Home shortcut.

**Preview & thumbnails**
- Built-in image preview: PNG / JPEG / BMP / ICO / TIFF / WebP / GIF.
- Thumbnail grid view.
- "Convert to PNG" dialog (real compression level).

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

### Usage

Just launch `MarinExplorer.exe`. The first open reads and caches disk info; afterwards it's fast.

💡 **Run as Administrator** is recommended: it avoids permission errors when reaching protected system folders, writing to system paths, or safely ejecting removable drives.

🔒 **Security**: only download `MarinExplorer.exe` from this repo's **Releases** or official build artifacts. Do not fetch it from unknown sources — that's how trojans and backdoors get in.

### License

[MIT](./LICENSE) © 2026 Rei1mu

Free to use, modify, and redistribute — just keep the copyright notice.
