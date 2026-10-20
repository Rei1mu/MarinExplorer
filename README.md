# MarinExplorer

一个**轻量、跨平台、基于 [egui](https://github.com/emilk/egui) 的双栏文件管理器**。

设计目标只有一条：**普通人也能顺手用**。没有华丽的架构，没有学习成本，打开就能用。

---

## 项目由来（Vibe Coding）

MarinExplorer 是一个典型的 **vibe coding** 产物——它不是一个先画好架构图再填代码的工程，而是在 AI 结对的快速迭代里「长」出来的。

起点只是很朴素的一个念头：系统自带的文件管理器要么太重、要么不顺手，而主流第三方工具又塞了太多用不上的功能。于是就这么一句话开始，在 AI 辅助下一次次打磨：

> 「这个按钮点了没反应」
> 「图标带黑底，alpha 没取对」
> 「回收站一打开就卡死」
> 「这行字太灰，看不清」

代码里保留了大量中文注释，因为作者和 AI 全程用中文交流。功能优先级永远是**易用性 > 精度**：宁可牺牲一点严谨，也要让第一次打开的人不用看说明书。

当前形态：一个基于 Rust + egui 的桌面文件管理器，专注「能用、顺手」而非功能堆砌。

---

## 功能（Features）

### 双栏浏览
- 左右两个**互相独立**的文件面板，各自维护：当前目录、筛选、排序、历史。
- 顶部工具栏：`返回 / 前进 / 上级 / 刷新 / 复制 / 移动 / 粘贴 / 删除 / 新建文件夹 / 跳转 / 终端 / 显示隐藏文件`。

### 侧边栏
- 回收站入口（列表后台异步加载，不卡 UI；清空后自动刷新，图标随满/空状态变化）。
- 固定磁盘列表，带**剩余 / 总量**空间显示。
- 主页（Home）快捷入口。

### 预览与缩略图
- 内置图片预览面板，支持 PNG / JPEG / BMP / ICO / TIFF / WebP / GIF。
- 缩略图网格视图。
- 「转换为 PNG」对话框（PNG 编码带真实压缩级别）。

### 拖拽与外壳集成
- 从面板**拖出文件**到外部程序。
- 使用系统**真实文件图标**（shell icon），而不是自己画一套。
- 支持 Windows 原生：深色标题栏、应用图标、系统托盘常驻。

### 性能与体验
- 文件夹大小在**后台线程**计算，并持久化到磁盘（`%APPDATA%\rust-explorer\store.txt`），重开即秒显。
- 底部状态栏：状态、当前路径、条目数、任务进度。
- 多语言界面：**简体中文 / English / 日本語 / 繁體中文**。
- 亮色 / 暗色主题；中文字体回退（Noto Sans SC），中文不乱码。

### 键盘
`↑/↓` 选择 · `Enter` 打开 · `Home/End` 跳首尾 · `Delete` 删除 · `Alt+←/→` 后退/前进 · `Esc` 关闭弹窗。

---

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

---

## 运行（Usage）

直接启动 `MarinExplorer.exe`。首次打开会读取并缓存磁盘信息，之后都很快。

---

## 许可证（License）

[MIT](./LICENSE) © 2026 Rei1mu

自由使用、修改、再分发，只需保留版权声明。
