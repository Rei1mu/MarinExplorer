# MarinExplorer

[中文](#中文) · [English](#english)

一个轻量、跨平台、基于 [egui](https://github.com/emilk/egui) 的**双栏文件管理器**。
设计目标只有一条：**普通人也能顺手用**——没有华丽架构，没有学习成本，打开就能用。

---

<a id="中文"></a>

# 中文文档

## 项目由来（Vibe Coding）

MarinExplorer 是一个典型的 **vibe coding** 产物：不是在白板上画好架构再填代码，而是在 AI 结对中「长」出来的。

起点很朴素：系统自带的文件管理器（Windows 资源管理器）**太占内存、太卡，连回收站都经常回收不上**——想找回误删的文件反而更麻烦。于是冒出一个念头：能不能有个**轻、快、普通人也能顺手用**的替代品？就这一句话，在 AI 辅助下一次次打磨成了现在的样子。

> 「这个按钮点了没反应」
> 「图标带黑底，alpha 没取对」
> 「回收站一打开就卡死」
> 「这行字太灰，看不清」

代码里保留了大量中文注释，因为作者和 AI 全程用中文交流。功能优先级永远是**易用性 > 精度**：宁可牺牲一点严谨，也要让第一次打开的人不用看说明书。

当前形态：一个基于 Rust + egui 的桌面文件管理器，专注「能用、顺手」而非功能堆砌。

## 功能（Features）

### 双栏浏览
- 左右两个互相独立的文件面板，各自维护：当前目录、筛选、排序、历史。
- 顶部工具栏：`返回 / 前进 / 上级 / 刷新 / 复制 / 移动 / 粘贴 / 删除 / 新建文件夹 / 跳转 / 终端 / 显示隐藏文件`。

### 侧边栏
- 回收站入口（列表后台异步加载，不卡 UI；清空后自动刷新，图标随满/空状态变化）。
- 固定磁盘列表，带剩余 / 总量空间显示。
- 主页（Home）快捷入口。

### 预览与缩略图
- 内置图片预览面板，支持 PNG / JPEG / BMP / ICO / TIFF / WebP / GIF。
- 缩略图网格视图。
- 「转换为 PNG」对话框（PNG 编码带真实压缩级别）。

### 拖拽与外壳集成
- 从面板拖出文件到外部程序。
- 使用系统真实文件图标（shell icon），而不是自己画一套。
- 支持 Windows 原生：深色标题栏、应用图标、系统托盘常驻。

### 性能与体验
- 文件夹大小在后台线程计算，并持久化到磁盘（`%APPDATA%\rust-explorer\store.txt`），重开即秒显。
- 底部状态栏：状态、当前路径、条目数、任务进度。
- 多语言界面：简体中文 / English / 日本語 / 繁體中文。
- 亮色 / 暗色主题；中文字体回退（Noto Sans SC），中文不乱码。

### 键盘
`↑/↓` 选择 · `Enter` 打开 · `Home/End` 跳首尾 · `Delete` 删除 · `Alt+←/→` 后退/前进 · `Esc` 关闭弹窗。

## 构建（Build）

需要 Rust 工具链与（Windows 下）MinGW-w64。

```bash
# 默认目标
cargo build --release

# 本项目的 Windows gnu 目标（见 .cargo/config.toml，已配好 MinGW 链接器）
cargo build --release --target x86_64-pc-windows-gnu
```

产物：`target/release/MarinExplorer.exe`（gnu 目标在 `target/x86_64-pc-windows-gnu/release/`）。

- 发布构建已为体积优化（`opt-level = "z"` + LTO + strip）。
- 应用图标由 `build.rs` 把 `logo.ico` 编译进 Windows 资源。
- 代码签名（可选）见 `sign.ps1`：用受信任 CA 的 `.pfx` 给 exe 签名 + 时间戳，消除 SmartScreen 拦截。

## 运行（Usage）

直接启动 `MarinExplorer.exe`。首次打开会读取并缓存磁盘信息，之后都很快。

> **💡 建议以管理员身份运行**
> 为获得最佳体验（如访问受保护目录、写入系统路径、正常弹出 / 卸载移动设备），建议右键 `MarinExplorer.exe` 选择「以管理员身份运行」。

> **🔒 安全提醒**
> 请仅从本仓库的 Releases 或官方构建获取 `MarinExplorer.exe`。**不要从不明渠道下载或运行来历不明的 exe 二进制文件**，以免被植入木马或后门。

## 许可证（License）

[MIT](./LICENSE) © 2026 Rei1mu

自由使用、修改、再分发，只需保留版权声明。

---

<a id="english"></a>

# English

A **lightweight, cross-platform, [egui](https://github.com/emilk/egui)-based two-pane file manager**.
One design goal only: **usable by ordinary people out of the box** — no fancy architecture, no learning curve, just open and use.

## Project Origin (Vibe Coding)

MarinExplorer is a typical **vibe coding** artifact: not a project drawn on a whiteboard and then filled with code, but one that "grew" through AI pair-programming.

The starting point was simple: the system's built-in file manager (Windows Explorer) is **too memory-hungry and too laggy, and its Recycle Bin often fails to restore files** — recovering an accidentally deleted file becomes even more troublesome. So the idea emerged: couldn't there be a **light, fast alternative that ordinary people can use intuitively**? From that one sentence, it was refined step by step with AI assistance into what it is today.

> "This button does nothing when clicked."
> "The icon has a black background — the alpha wasn't handled right."
> "The Recycle Bin freezes on open."
> "This text is too gray to read."

The code keeps a lot of Chinese comments because the author and the AI communicate entirely in Chinese. The priority is always **usability > precision**: we'd rather sacrifice a bit of rigor than make a first-time user read a manual.

Current form: a Rust + egui desktop file manager focused on "works and feels right" rather than feature padding.

## Features

### Two-pane browsing
- Two independent file panels (left/right), each maintaining its own directory, filter, sort, and history.
- Top toolbar: `Back / Forward / Up / Refresh / Copy / Move / Paste / Delete / New Folder / Jump / Terminal / Show Hidden`.

### Sidebar
- Recycle Bin entry (list loaded asynchronously in the background, no UI freeze; auto-refreshes after emptying; icon reflects full/empty state).
- Fixed disk list with free / total space display.
- Home shortcut entry.

### Preview & thumbnails
- Built-in image preview panel supporting PNG / JPEG / BMP / ICO / TIFF / WebP / GIF.
- Thumbnail grid view.
- "Convert to PNG" dialog (real PNG compression levels).

### Drag & shell integration
- Drag files out of a panel into external programs.
- Uses the system's real file icons (shell icons) instead of drawing its own set.
- Windows-native: dark title bar, app icon, system tray residency.

### Performance & experience
- Folder sizes computed on a background thread and persisted to disk (`%APPDATA%\rust-explorer\store.txt`), shown instantly on restart.
- Bottom status bar: status, current path, item count, task progress.
- Multi-language UI: Simplified Chinese / English / 日本語 / 繁體中文.
- Light / dark theme; Chinese font fallback (Noto Sans SC), no mojibake.

### Keyboard
`↑/↓` select · `Enter` open · `Home/End` jump to first/last · `Delete` delete · `Alt+←/→` back/forward · `Esc` close dialog.

## Build

Requires the Rust toolchain and (on Windows) MinGW-w64.

```bash
# default target
cargo build --release

# this project's Windows gnu target (see .cargo/config.toml, MinGW linker preconfigured)
cargo build --release --target x86_64-pc-windows-gnu
```

Artifact: `target/release/MarinExplorer.exe` (gnu target in `target/x86_64-pc-windows-gnu/release/`).

- Release build is size-optimized (`opt-level = "z"` + LTO + strip).
- App icon is compiled into Windows resources by `build.rs` from `logo.ico`.
- Code signing (optional) — see `sign.ps1`: sign the exe + timestamp with a trusted CA `.pfx` to clear SmartScreen.

## Usage

Just launch `MarinExplorer.exe`. On first open it reads and caches disk info; afterwards everything is fast.

> **💡 Run as Administrator (recommended)**
> For the best experience (e.g., accessing protected directories, writing to system paths, or safely ejecting removable drives), right-click `MarinExplorer.exe` and choose "Run as administrator".

> **🔒 Security notice**
> Only obtain `MarinExplorer.exe` from this repository's Releases or official builds. **Do not download or run exe binaries from untrusted sources**, as they may contain trojans or backdoors.

## License

[MIT](./LICENSE) © 2026 Rei1mu

Free to use, modify, and redistribute, provided the copyright notice is retained.
