# MarinExplorer

一个**轻量、支持 Windows on ARM64、基于 Rust + [egui](https://github.com/emilk/egui) 的双栏文件管理器**。

A **lightweight, Windows-on-ARM64-capable, Rust + [egui](https://github.com/emilk/egui) based two-pane file manager**.

**仅支持 Windows**——`x86_64`（标准 Windows 64 位）与 `aarch64`（Windows on ARM64，如 Surface / 骁龙笔记本）。不提供 Linux 或 macOS 版本。

**Windows only** — `x86_64` (standard 64-bit Windows) and `aarch64` (Windows on ARM64, e.g. Surface / Snapdragon laptops). No Linux or macOS builds.

设计目标只有一条：**普通人也能顺手用**。没有华丽的架构，没有学习成本，打开就能用。

The only design goal: **usable by anyone, out of the box**. No fancy architecture, no learning curve — open it and it just works.

---

## 项目由来（Vibe Coding）

MarinExplorer 是一个典型的 **vibe coding** 产物——它不是一个先画好架构图再填代码的工程，而是在 AI 结对的快速迭代里「长」出来的。

MarinExplorer is a textbook **vibe coding** artifact — not a project that started from an architecture diagram, but one that *grew* out of rapid, AI-paired iteration.

起点只是很朴素的一个念头：系统自带的文件管理器要么太重、要么不顺手，而主流第三方工具又塞了太多用不上的功能。于是就这么一句话开始，在 AI 辅助下一次次打磨：

It started from a simple thought: the built-in file manager is either too heavy or too clunky, while most third-party tools pile on features nobody asked for. From that one line, it was refined again and again with AI assistance:

> 「这个按钮点了没反应」
> "This button does nothing when I click it"
> 「图标带黑底，alpha 没取对」
> "The icon has a black background — wrong alpha"
> 「回收站一打开就卡死」
> "The Recycle Bin freezes the moment it opens"
> 「这行字太灰，看不清」
> "This text is too grey, I can't read it"

代码里保留了大量中文注释，因为作者和 AI 全程用中文交流。功能优先级永远是**易用性 > 精度**：宁可牺牲一点严谨，也要让第一次打开的人不用看说明书。

The code keeps plenty of Chinese comments because the author and the AI talked in Chinese the whole way. Feature priority is always **usability > precision**: we'd rather give up a little rigour than make a first-time user read a manual.

当前形态：一个基于 Rust + egui 的 Windows 桌面文件管理器，专注「能用、顺手」而非功能堆砌，并原生支持 Windows on ARM64（msvc）。

Current form: a Rust + egui Windows desktop file manager focused on "works and feels right" rather than feature count, with native support for Windows on ARM64 (MSVC).

---

## 功能（Features）

### 双栏浏览 / Two-pane browsing
- 左右两个**互相独立**的文件面板，各自维护：当前目录、筛选、排序、历史。
  Two **independent** panels, each keeping its own directory, filter, sort order and history.
- 顶部工具栏：`返回 / 前进 / 上级 / 刷新 / 复制 / 移动 / 粘贴 / 删除 / 新建文件夹 / 跳转 / 终端 / 显示隐藏文件`。
  Toolbar: back / forward / up / refresh / copy / move / paste / delete / new folder / jump / terminal / show hidden.

### 侧边栏 / Sidebar
- 回收站入口（列表后台异步加载，不卡 UI；清空后自动刷新，图标随满/空状态变化）。
  Recycle Bin entry (loaded async in the background; auto-refreshes after emptying, icon reflects full/empty).
- 固定磁盘列表，带**剩余 / 总量**空间显示。
  Fixed drive list with **free / total** space.
- 主页（Home）快捷入口。
  Home shortcut.

### 预览与缩略图 / Preview & thumbnails
- 内置图片预览面板，支持 PNG / JPEG / BMP / ICO / TIFF / WebP / GIF。
  Built-in image preview: PNG / JPEG / BMP / ICO / TIFF / WebP / GIF.
- 缩略图网格视图。
  Thumbnail grid view.
- 「转换为 PNG」对话框（PNG 编码带真实压缩级别）。
  "Convert to PNG" dialog (real compression level).

### 拖拽与外壳集成 / Drag & shell integration
- 从面板**拖出文件**到外部程序。
  Drag files **out** of a panel into other apps.
- 使用系统**真实文件图标**（shell icon），而不是自己画一套。
  Uses the system's **real file icons** (shell icons), not a hand-drawn set.
- 支持 Windows 原生：深色标题栏、应用图标、系统托盘常驻。
  Native Windows: dark title bar, app icon, system tray resident.

### 性能与体验 / Performance & UX
- 文件夹大小在**后台线程**计算，并持久化到磁盘（`%APPDATA%\rust-explorer\store.txt`），重开即秒显。
  Folder sizes computed in a **background thread** and persisted (`%APPDATA%\rust-explorer\store.txt`) for instant reload.
- 底部状态栏：状态、当前路径、条目数、任务进度。
  Status bar: state, current path, item count, task progress.
- 多语言界面：**简体中文 / English / 日本語 / 繁體中文**。
  UI languages: Simplified Chinese / English / Japanese / Traditional Chinese.
- 亮色 / 暗色主题；中文字体回退（Noto Sans SC），中文不乱码。
  Light / dark theme; CJK font fallback (Noto Sans SC) so Chinese never mojibakes.

### 键盘 / Keyboard
`↑/↓` 选择 · `Enter` 打开 · `Home/End` 跳首尾 · `Delete` 删除 · `Alt+←/→` 后退/前进 · `Esc` 关闭弹窗。
`↑/↓` select · `Enter` open · `Home/End` jump to top/bottom · `Delete` delete · `Alt+←/→` back/forward · `Esc` close dialog.

---

## 构建（Build）

需要 Rust 工具链。本项目只产出 **Windows** 可执行文件，两个目标：

Requires the Rust toolchain. This project builds **Windows** executables only, for two targets:

- `x86_64-pc-windows-gnu` —— 标准 Windows x64，使用 MinGW-w64。
  Standard Windows x64, built with MinGW-w64.
- `aarch64-pc-windows-msvc` —— Windows on ARM64（如 Surface / 骁龙笔记本），使用 MSVC。
  Windows on ARM64 (e.g. Surface / Snapdragon laptops), built with MSVC.

```bash
# 默认目标 / default target
cargo build --release

# Windows x64（gnu）
cargo build --release --target x86_64-pc-windows-gnu

# Windows on ARM64（msvc）
cargo build --release --target aarch64-pc-windows-msvc
```

产物 / Artifacts:
- `target/x86_64-pc-windows-gnu/release/MarinExplorer.exe`
- `target/aarch64-pc-windows-msvc/release/MarinExplorer.exe`

说明 / Notes:
- 发布构建已为体积优化（`opt-level = "z"` + LTO + strip）。
  Release builds are size-optimized (`opt-level = "z"` + LTO + strip).
- 应用图标由 `build.rs` 把 `logo.ico` 编译进 Windows 资源（仅 x86_64-gnu；arm64-msvc 构建不带内嵌文件图标，但不影响运行）。
  The app icon is embedded via `build.rs` (x86_64-gnu only; the arm64-msvc build ships without a bundled file icon, but runs fine).
- 代码签名（可选）见 `sign.ps1`：用受信任 CA 的 `.pfx` 给 exe 签名 + 时间戳，消除 SmartScreen 拦截。
  Optional code signing lives in `sign.ps1`.

---

## 🤖 自动构建（GitHub Actions / Automated Builds）

仓库已在每次推送 `main` 时通过 GitHub Actions 自动编译 **两个 Windows 版本**（x86_64 在 Linux 上用 llvm-mingw 交叉编译，arm64 在 Windows runner 上用原生 MSVC 交叉编译，产物全是 Windows `.exe`）：

On every push to `main`, GitHub Actions automatically builds **both Windows editions** (x86_64 is cross-compiled on Linux with llvm-mingw; arm64 is cross-compiled on a Windows runner with the native MSVC tools — every artifact is a Windows `.exe`):

- `x86_64-pc-windows-gnu`（标准 Windows x64）
- `aarch64-pc-windows-msvc`（Windows on ARM64）

工作流见 [`.github/workflows/build.yml`](.github/workflows/build.yml)。两个产物名为 `MarinExplorer-x86_64.exe` 与 `MarinExplorer-arm64.exe`，可在 **Actions → 对应运行 → Artifacts** 下载；打 `v*` 标签推送时会自动发布到 **Releases**。

Workflow: [`.github/workflows/build.yml`](.github/workflows/build.yml). The two artifacts are `MarinExplorer-x86_64.exe` and `MarinExplorer-arm64.exe`, downloadable from **Actions → run → Artifacts**; pushing a `v*` tag also publishes them to **Releases**.

---

## 运行（Usage）

直接启动 `MarinExplorer.exe`。首次打开会读取并缓存磁盘信息，之后都很快。

Just launch `MarinExplorer.exe`. The first open reads and caches disk info; afterwards it's fast.

💡 **建议以管理员身份运行**：访问受保护的系统目录、写入系统路径、以及正常弹出/弹出移动设备时，管理员权限能避免权限不足导致的功能受限。

💡 **Run as Administrator** is recommended: it avoids permission errors when reaching protected system folders, writing to system paths, or safely ejecting removable drives.

🔒 **安全提醒**：请只从本仓库的 **Releases** 或官方构建渠道获取 `MarinExplorer.exe`。不要从不明来源下载，以免被植入木马或后门。

🔒 **Security**: only download `MarinExplorer.exe` from this repo's **Releases** or official build artifacts. Do not fetch it from unknown sources — that's how trojans and backdoors get in.

---

## 许可证（License）

[MIT](./LICENSE) © 2026 Rei1mu

自由使用、修改、再分发，只需保留版权声明。

Free to use, modify, and redistribute — just keep the copyright notice.
