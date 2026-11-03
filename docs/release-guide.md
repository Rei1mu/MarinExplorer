# 发版流程指南

MarinExplorer（含其他 GitHub + Actions 项目的通用发版流程）。
整理自 2026-11-02 的 v0.1.17 实操，踩过的坑都在里面。

---

## 一、七条铁律

1. **推分支 ≠ 发布。**
   绝大多数 CI 的发布 job 写着 `if: startsWith(github.ref, 'refs/tags/v')`——push 分支只构建产物，**不会上传资产**。tag 才是发布的唯一开关。

2. **tag 必须打在版本提交之后。**
   先 commit 版本号改动，再 `git tag -a` 同一个 commit。否则 Release 对应的是旧代码。

3. **直接推 `main`，不要建 release 分支。**
   2026-11-02 明确纠正过。多余分支污染远端，且要的永远是 `main` 上的最新版本。

4. **版本号有多处声明，漏一处就装出旧版本。**
   `Cargo.toml` / `package.json` / `pyproject.toml` / `*.iss` / `VERSION` / `*.csproj` / `tauri.conf.json` / `pubspec.yaml`。
   装包类项目漏掉 `.iss` 尤其致命——安装器自己会显示旧版本号。

5. **本地 build 过 ≠ CI 过。**
   CI 常带交叉编译、静态 CRT、矩阵架构、额外打包。

6. **本地 exe ≠ Release 资产。**
   Release 里的文件是 CI 构建的，可能是别的架构或别的工具链产物，别拿本地文件当交付物。

7. **GitHub 匿名 API 配额只有 60/小时，且本机直连常被限流。**
   查状态走代理 `http://127.0.0.1:7890` + token（见第六节）。

---

## 二、完整流程（8 步）

### 1. 侦察

先确认三件事：**工作区干净**、**版本号声明位置**、**CI 的发布触发条件**。

```bash
git status --short                       # 必须干净
git log --oneline -1
git tag --sort=-v:refname | head -5      # 最近 tag，确认不撞车
grep -rn "refs/tags\|tags:" .github/workflows/    # 发布 job 认什么触发
```

版本号位置扫一遍（按项目类型挑）：

```bash
grep -n '^version' Cargo.toml
grep -rn 'MyAppVersion' installer/*.iss
grep -n '"version"' package.json
grep -n '^version' pyproject.toml
```

### 2. 同步版本号

改**所有**声明位置。以 v0.1.17 → v0.1.18 为例：

```bash
# Cargo.toml：只改 [package] 段，别碰 [dependencies]
#   [package]
#   version = "0.1.18"

# installer/marin.iss：装包版本号，独立一份
#   #define MyAppVersion "0.1.18"
```

改完 `git diff` 复核一遍。脚本覆盖不到的自研版本声明（`app_version()` 函数、常量表）要手动补。

### 3. 更新 CHANGELOG

- 有 `CHANGELOG.md` → 顶部加本次版本条目（中英双语）。
- 没有 → 看 README 里的 changelog 段落。
- 都没有则跳过，但最终汇报里说明"本次未维护变更日志"。

### 4. 构建验证

```bash
cargo build --release
```

Windows + MSVC 要先设环境变量，否则 linker 报错：

```bash
export PATH="C:/Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/14.44.35207/bin/HostX64/x64:$PATH"
export LIB="C:/Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/14.44.35207/lib/x64;D:/d/winsdk_libs"
```

**不要** `cargo clean`、不要删 `target/`——那只是浪费时间。

### 5. 提交并推送

```bash
git add Cargo.toml Cargo.lock installer/marin.iss README.md CHANGELOG.md src/
git commit -m "chore: bump version 0.1.17 -> 0.1.18"
git push origin main
```

- commit message 带版本号，方便回溯。
- 推送前确认 diff 只含本次发版内容。
- `main` 没有 upstream 时：`git branch --set-upstream-to=origin/main main`。

### 6. 打 tag 并推送

```bash
git tag -a v0.1.18 -m "MarinExplorer v0.1.18"
git push origin v0.1.18
```

- tag 格式必须匹配 CI 的 `tags: ['v*']`（CI 只认 `v` 前缀就别用裸版本号）。
- `git tag -a` 是 annotated tag，**Release 页标题取它的 message**。
- 先查远端有没有同名 tag：`git ls-remote --tags origin | grep v0.1.18`。

### 7. 等待 CI 并校验

CI 通常 3–6 分钟（两个架构 build + Inno Setup 打包 + 上传）。

```bash
# 看最近 run 状态
curl -s -x http://127.0.0.1:7890 \
  "https://api.github.com/repos/Rei1mu/MarinExplorer/actions/runs?per_page=6" \
  | python -c "import json,sys;[print(r['created_at'],r['head_branch'],r['status'],r['conclusion']) for r in json.load(sys.stdin)['workflow_runs']]"

# 看 Release 与资产
curl -s -x http://127.0.0.1:7890 \
  "https://api.github.com/repos/Rei1mu/MarinExplorer/releases/tags/v0.1.18" \
  | python -c "import json,sys;d=json.load(sys.stdin);print(d['tag_name'],d['published_at']);[print(' ',a['name'],a['size'],a.get('digest'),a['browser_download_url']) for a in d['assets']]"
```

轮询节流：**间隔 ≥60 秒，一次只查一个 endpoint**。一次发布轮询下来足够把 60 次匿名配额打光。

### 8. 汇报

给出：版本号与 tag、commit hash、CI run 结论、Release 链接、**每个资产的名字 + 大小 + sha256 + 下载直链**、需要用户自己实测的部分。

体积变化要解释原因（新增依赖、换了渲染器等），别让用户以为是 bug。

---

## 三、版本号位置对照表

| 生态 / 产物 | 文件 | 字段 |
|---|---|---|
| Rust | `Cargo.toml` | `[package]` 段的 `version = "…"`（**不要**改 `[dependencies]` 里的） |
| Rust workspace | `Cargo.toml` | `[workspace.package]` 的 `version` |
| Node | `package.json` | 顶层 `"version"`（锁文件里也有一份，需一并更新） |
| Python (PEP 621) | `pyproject.toml` | `project.version` / `tool.poetry.version` |
| Python (legacy) | `setup.py` / `setup.cfg` | `version=` |
| Dart / Flutter | `pubspec.yaml` | `version:` |
| Tauri | `tauri.conf.json` | `"version"` |
| Gradle | `build.gradle` / `build.gradle.kts` | `version = '…'` |
| .NET | `*.csproj` / `Directory.Build.props` | `<Version>…</Version>` |
| Go | `cmd/*/version.go` | 常量（手动改） |
| **Inno Setup 装包** | `installer/*.iss` | `#define MyAppVersion "…"` |
| NSIS 装包 | `installer/*.nsi` | `!define VERSION "…"` |
| WiX | `Product.wxs` | `Version="…"` |
| 纯文本 | `VERSION` / `VERSION.txt` | 整行版本号 |

**装包类项目的致命坑**：安装器里的版本号是独立一份声明。只改程序本体、没改装包脚本 → 用户装出来看到的是旧版本。

---

## 四、CI 发布模式

### A. tag 触发（最常见）

```yaml
on:
  push:
    branches: [main]
    tags: ['v*']
jobs:
  build: …
  publish:
    if: startsWith(github.ref, 'refs/tags/v')   # ← 关键
    needs: [build]
```

push 分支：只跑 build，产物停在 artifact，**不发布**。
push tag：跑完整链路 → 创建 Release + 上传资产。

### B. 分支触发发布

```yaml
on:
  push:
    branches: [main]
jobs:
  publish:
    if: github.ref == 'refs/heads/main'
```

每次推 main 都发一次。这种模式下 tag 只影响版本号展示。

### C. 矩阵 + 单一发布 job（推荐）

```yaml
jobs:
  build:
    strategy:
      matrix:
        target: [x86_64-pc-windows-msvc, aarch64-pc-windows-msvc]
  publish:
    needs: [build]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v7
        with: { path: dist, merge-multiple: true }
      - uses: softprops/action-gh-release@v3
        with: { files: dist/*Setup*.exe }
```

**多个 job 各自上传同一 Release 会互相覆盖**，导致随机丢资产（真实踩过：arm64 被 x64 覆盖掉）。正确做法是一个 job 收齐全部 artifact 统一上传。

### D. 交叉编译注意

- Rust → Windows ARM64：`aarch64-pc-windows-msvc` + `vcvarsall x64_arm64`，`-C target-feature=+crt-static` 免装 VC++ 运行库。
- Inno Setup 7 原生支持 `arm64`；**Inno 6 缺中文语言文件**（`Languages\ChineseSimplified.isl`），装包会报缺语言。CI 里用 winget 装 Inno Setup 7。

---

## 五、等不到 Release 时的排查表

| 现象 | 原因 | 处理 |
|---|---|---|
| Release 404，runs 里没有 tag 分支的 run | tag 没推上去 | `git push origin v0.1.18` |
| tag 的 run 存在但 publish job 被 skip | publish job 的 `if` 没匹配 | 看 workflow 的 `if:`，改 tag 格式或改 workflow |
| Release 出来了但没有资产 | upload step 失败 / 资产名不匹配 `files:` glob | 看该 run 的 publish job 日志 |
| 只有一半资产（只有 x64 没有 arm64） | 多 job 并行上传互相覆盖 | 收敛成单一 publish job |
| 安装器里显示旧版本号 | 漏改 `.iss` / 版本文件 | 补齐后重打 tag（`git tag -f`） |
| Release 有资产但版本号是旧的 | tag 打在版本提交之前 | 删 tag 重打 |

---

## 六、GitHub 状态查询的四种途径

| 途径 | 优点 | 坑 |
|---|---|---|
| `gh` CLI | 最好用 | 本机通常没装 |
| REST API + token | 配额 5000/h | 需 token |
| REST API 匿名 | 无需凭据 | 配额 60/h，本机直连常被限流；需代理 `http://127.0.0.1:7890` |
| WebFetch 工具 | 绕开本机网络与配额 | 返回的是摘要不是原始 JSON |

**从 Windows 凭据管理器取 token**（GCM 存 git 凭证处）：

```python
import ctypes
from ctypes import wintypes

class CREDENTIALW(ctypes.Structure):
    _fields_ = [
        ("Flags", wintypes.DWORD), ("Type", wintypes.DWORD),
        ("TargetName", wintypes.LPWSTR), ("Comment", wintypes.LPWSTR),
        ("LastWritten", wintypes.FILETIME),
        ("CredentialBlobSize", wintypes.DWORD),
        ("CredentialBlob", ctypes.POINTER(ctypes.c_ubyte)),
        ("Persist", wintypes.DWORD), ("AttributeCount", wintypes.DWORD),
        ("Attributes", ctypes.c_void_p), ("TargetAlias", wintypes.LPWSTR),
        ("UserName", wintypes.LPWSTR),
    ]

advapi = ctypes.windll.advapi32
pcred = ctypes.POINTER(CREDENTIALW)()
advapi.CredReadW("git:https://github.com", 1, 0, ctypes.byref(pcred))
blob = ctypes.string_at(pcred.contents.CredentialBlob, pcred.contents.CredentialBlobSize)
token = blob.decode("utf-16-le").strip("\x00")
advapi.CredFree(pcred)
```

用法：`curl -s -x http://127.0.0.1:7890 -H "Authorization: Bearer $token" https://api.github.com/...`

**限流表现**：HTTP 403 且响应头 `X-RateLimit-Remaining: 0`，体是 `{"message": "API rate limit exceeded"}`。`X-RateLimit-Reset` 是恢复的 Unix 时间戳。

---

## 七、发布后校验清单

- [ ] Release 的 `tag_name` / `name` 与预期版本一致
- [ ] `published_at` 有值（不是 draft）
- [ ] 资产数量与架构数匹配（x64 + arm64 = 2 个）
- [ ] 每个资产 `size` 非零、`state == "uploaded"`
- [ ] 资产文件名带正确版本号（`…-0.1.18.exe`）
- [ ] 记录 `digest`（sha256），便于校验与分发
- [ ] workflow run 的 `conclusion == "success"`，publish job 未被 skip
- [ ] 安装器版本号 = 程序版本号
- [ ] 本地版本号文件与 tag 一致

---

## 八、失败重来

```bash
# tag 打错了：先删本地再删远端
git tag -d v0.1.18
git push origin :refs/tags/v0.1.18

# 版本号写错要重来：强制重打
git tag -f v0.1.18
git push origin v0.1.18 --force
```

**不要**为了让 CI 变绿去改 workflow 跳过检查——修产物，不修流水线。

---

## 九、本项目实例（v0.1.17，2026-11-02）

| 项 | 值 |
|---|---|
| commit | `ed841b6`（版本+文档+预览缓存修复）、`2b04b0b`（.iss 版本） |
| tag | `v0.1.17` → `2b04b0b` |
| CI | main run + v0.1.17 tag run 均 success（07:19 起） |
| Release | published 2026-11-02T07:25:50Z，body 为空（与历史一致） |
| 资产 | x64-msvc 5,868,771B（sha256 `a6c20338…`）、arm64 5,576,235B（sha256 `bf97d384…`） |
| 体积变化 | x64 4.34MB → 5.87MB，＝PDF 渲染器编进 exe，正常 |
| 本地部署 | `MarinExplorer_new.exe` = 10,003,456B |
