# 更新日志 / Changelog

> MarinExplorer 的全部版本记录。最新功能特性见 [README.md](./README.md#功能)。

### v0.1.19 — 2026-11-04
- **修复 ARM64 构建失败**：v0.1.18 的 `build.rs` 在 `rc.exe` 缺失时回退到 MinGW `windres`，产出的 x86_64 对象被 ARM64 链接器拒绝（`LNK1112: module machine type 'x64' conflicts with target machine type 'ARM64'`）。现已**彻底移除 windres / MinGW / gnu 回退**，仅用 MSVC + `rc.exe`；`rc.exe` 缺失时直接跳过资源，ARM64 构建恢复通过。

**English**
- **Fixed ARM64 build failure**: v0.1.18's `build.rs` fell back to MinGW `windres` when `rc.exe` was missing, emitting an x86_64 object that the ARM64 linker rejected (`LNK1112: module machine type 'x64' conflicts with target machine type 'ARM64'`). The `windres` / MinGW / `gnu` fallback is now **removed entirely** — MSVC + `rc.exe` only; if `rc.exe` is missing the resource is skipped and the ARM64 build passes again.

### v0.1.18 — 2026-11-04
- **版本号写入 exe**：文件属性 → 详细信息现在显示 `FileVersion 0.1.18`（此前为空，无法区分运行版本）。图标与版本资源仅用 Windows SDK 的 `rc.exe` 编译（MSVC only，无 MinGW / windres 回退；`rc.exe` 缺失时自动跳过资源，构建照常通过）。
- **修复双栏分隔条 panic**：窗口非常窄时分隔比例 `clamp` 下界大于上界会触发 panic，已封顶为 0.5。

**English**
- **Version stamped into the exe**: File Explorer → properties now shows `FileVersion 0.1.18` (previously blank, so you could not tell which build was running). Icon and version resources compile with the Windows SDK `rc.exe` only (MSVC only, no MinGW / windres fallback; if `rc.exe` is missing the resource is skipped and the build still passes).
- **Fixed split-pane panic**: on a very narrow window the split ratio `clamp` lower bound exceeded the upper bound and panicked; now capped at 0.5.

### v0.1.17 — 2026-11-01
- **文件右键「转换为 ICO / PNG」**：选中图片在文件列表/网格右键即可转换，不必先打开预览窗口。
- **预览窗口激活前置**：已打开预览时再双击另一个图片/gif/视频，原窗口前置并聚焦，不再需要手动切回。
- **PDF 预览**：预览窗格、网格缩略图、全屏预览均支持 PDF（渲染首页）；纯 Rust 渲染器编译进 exe，无需外部 DLL。
- **预览随文件更新**：同名替换图片 / PDF（或点「刷新」）后，预览窗格、网格缩略图、全屏窗口会自动重解码，不再显示旧图。

**English**
- **Convert to ICO / PNG from the file right-click menu**: no need to open the viewer first.
- **Viewer pops to front**: double-clicking another image/gif/video while the viewer is open focuses the existing window instead of leaving it behind.
- **PDF preview**: the side pane, grid thumbnail and full viewer all preview PDFs (first page); rendered by a pure-Rust rasterizer compiled into the exe, no external DLL needed.
- **Previews track the file on disk**: replacing an image/PDF with the same name (or clicking 刷新) now re-decodes the preview pane, grid thumbnail and full viewer instead of showing the stale cached image.

### v0.1.16 — 2026-10-28
- **剪贴板复制/剪切/粘贴恢复**：UI 框架把 Ctrl+C/X/V 改写成剪贴板命令，现直接响应命令并补写文本格式，与资源管理器互通。
- **拖出文件稳定化**（在 UI 线程 COM 单线程套间内执行）、**Delete 弹确认框且支持键盘**、**左下角实时回显按键**。
- **默认普通用户权限运行**，仅少数操作按需提权；内部命令不再弹控制台窗口。

**English**
- **Clipboard copy/cut/paste restored**: the UI framework rewrites Ctrl+C/X/V into clipboard commands; the app now handles them directly and adds a text flavour so it interoperates with Explorer.
- **Drag-out made stable** (runs inside a single-threaded COM apartment), **Delete asks for confirmation and takes the keyboard**, **live key echo in the bottom-left**.
- **Runs as a normal user by default**; only a few actions ask for elevation on demand; internal commands no longer spawn console windows.

### v0.1.15 — 2026-10-28
- **修复文件拖拽到外部程序**：补上完整的 `IDropSource`，文件可真正拖到 QQ / 上传框 / 资源管理器。

**English**
- **Fixed dragging files out to other programs**: a proper `IDropSource` is now provided, so files can be dragged into QQ, upload boxes, Explorer, anything.

### v0.1.14 — 2026-10-28
- **不再自动提权**（普通用户权限，剪贴板/拖拽到外部程序可用）。
- **GIF / 视频复制成文件**、**查看器右键删除**、**滚轮方向修正**、**列表/网格可直接拖出文件**。

**English**
- **No more auto-elevation** (normal integrity; clipboard copy and drag-out to external programs work).
- **GIF / video copy as a file**, **viewer right-click delete**, **wheel direction fixed**, **drag files out of the list / grid**.

### v0.1.13 — 2026-10-26
- **视频播放器**（Media Foundation）、**GIF 播放器**、**性能模式（低/中/高）**、**播放 CPU 开销大幅下降**，并修复若干播放相关 bug。

**English**
- **Video player** (Media Foundation), **GIF player**, **performance mode (Low/Medium/High)**, **much lower playback CPU cost**, plus playback bug fixes.

### v0.1.12 — 2026-10-25
- **列表/网格视图虚拟化**：超大文件夹流畅；**双击打开不再整目录重读**。

**English**
- **Virtualized list / grid views** for huge folders; **double-click no longer re-reads the whole directory**.

### v0.1.11 — 2026-10-25
- **字体下拉读系统全部字体** + **默认字号调大** + **驱动器容量文案压缩**。

**English**
- **Font picker lists every system font** + **larger default font size** + **compact drive-capacity text**.

### v0.1.10 — 2026-10-24
- **预览/查看器不再卡死**（优先解码队列）；**查看器胶片条移到窗口底部**。

**English**
- **Previews / viewer no longer hang** (priority decode queue); **viewer filmstrip moved to the bottom**.

### v0.1.9 — 2026-10-23
- **修复 v0.1.8 安装失败**：修正 `DefaultGroupName`。

**English**
- **Fixed the v0.1.8 install failure**: corrected `DefaultGroupName`.

### v0.1.8 — 2026-10-23
- **安装包修复**：安装包改为提权运行，升级不再被占用文件卡住。

**English**
- **Installer fix**: the installer now runs elevated, so upgrades no longer stall on a locked file.

### v0.1.7 — 2026-10-23
- **单实例守卫**、**大小计算框常驻**、**启动更安静**。

**English**
- **Single-instance guard**, **persistent size box**, **quieter startup**.

### v0.1.6 — 2026-10-23
- **清空回收站重构**（流式自删，真实进度）；补充**下载指引**。

**English**
- **Recycle Bin empty reworked** (streaming delete with real progress); **download note** added.

### v0.1.5 — 2026-10-23
- 快速访问改系统真实图标；「移动」改「剪贴」；清空回收站改后台线程 + 动画进度条。

**English**
- Quick-access uses real system icons; toolbar "Move" renamed to "Cut"; Recycle Bin empty moved to a background thread with an animated progress bar.
