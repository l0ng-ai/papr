1. 启动开发环境

# 安装依赖
pnpm install

# 启动完整桌面应用（前端 Vite + Rust 后端一起启动）
pnpm tauri dev

这会同时启动：
- Vite 前端 dev server → http://localhost:1430（热更新端口 1431）
- Rust 后端编译并启动 Tauri 窗口

---
2. 调试前端（React/TypeScript）

方式一：Tauri 内置 DevTools

在 pnpm tauri dev 运行时，Tauri 窗口内置了 Chromium DevTools：

- Windows/Linux: 右键 → "检查元素"，或按 Ctrl + Shift + I
- macOS: 右键 → "检查元素"，或按 Cmd + Option + I

DevTools 支持：Elements、Console、Network、Sources（断点调试）、Performance、React DevTools 扩展。

方式二：独立浏览器调试

直接访问 http://localhost:1430，用 Chrome/Edge 打开，获得完整的浏览器 DevTools 体验。

▎ 注意: Tauri API 调用（invoke()）在纯浏览器中不可用，但可以调试纯 UI 逻辑。

方式三：VS Code 调试

在 .vscode/launch.json 中添加：

{
"version": "0.2.0",
"configurations": [
    {
    "name": "Tauri Frontend (Edge)",
    "type": "msedge",
    "request": "launch",
    "url": "http://localhost:1430",
    "webRoot": "${workspaceFolder}/src"
    }
]
}

启动 pnpm tauri dev 后，用此配置附加断点。

---
3. 调试后端（Rust）

方式一：日志调试

在 Rust 代码中使用 log 宏（项目已依赖 log crate）：

log::info!("feed count: {}", count);
log::warn!("fetch failed for {}: {}", url, err);
log::error!("db error: {:?}", e);

运行时设置环境变量启用日志：

# Windows (PowerShell)
$env:RUST_LOG="debug"; pnpm tauri dev

# Windows (cmd)
set RUST_LOG=debug && pnpm tauri dev

# macOS/Linux
RUST_LOG=debug pnpm tauri dev

日志级别：error < warn < info < debug < trace

方式二：Rust 断点调试（VS Code + CodeLLDB）

1. 安装 CodeLLDB (https://marketplace.visualstudio.com/items?itemName=vadimcn.vscode-lldb) 扩展
2. 先编译一次 debug 版本：
cd src-tauri && cargo build
3. 在 .vscode/launch.json 添加：
{
"name": "Tauri Backend (LLDB)",
"type": "lldb",
"request": "launch",
"cargo": {
    "args": ["build", "--manifest-path=src-tauri/Cargo.toml"]
},
"preLaunchTask": "ui:dev",
"program": "${workspaceFolder}/src-tauri/target/debug/papr",
"args": [],
"cwd": "${workspaceFolder}"
}
4. 在 commands.rs、db.rs 等文件中设置断点即可。

方式三：Cargo 测试

对于纯 Rust 逻辑（不依赖 Tauri 运行时），直接跑测试：

cd src-tauri
cargo test
cargo test -- --nocapture   # 打印 println! 输出
cargo test <test_name>       # 运行单个测试

---
4. 调试 IPC 边界（前后端通信）

这是最容易出问题的地方。前端通过 src/api.ts 调用 invoke()，后端在 commands.rs 处理。

查看 IPC 调用

在 DevTools Console 中，Tauri 会打印 invoke 调用（在 debug 模式下）。

常见 IPC 调试技巧

// 前端 api.ts 中的调用都可以这样捕获错误
try {
const result = await invoke("get_feeds", { folderId: null });
console.log("feeds:", result);
} catch (e) {
console.error("IPC error:", e);  // 会显示 { code, detail }
}

Tauri 命令日志

启动时 Tauri 会打印所有注册的命令。如果命令名拼写错误，会报 "command not found"。

---
5. 调试数据库

SQLite 数据库位置

┌─────────┬───────────────────────────────────────────────────────┐
│  平台   │                         路径                          │
├─────────┼───────────────────────────────────────────────────────┤
│ Windows │ %APPDATA%\com.thomas.papr\papr.db                     │
├─────────┼───────────────────────────────────────────────────────┤
│ macOS   │ ~/Library/Application Support/com.thomas.papr/papr.db │
├─────────┼───────────────────────────────────────────────────────┤
│ Linux   │ ~/.local/share/com.thomas.papr/papr.db                │
└─────────┴───────────────────────────────────────────────────────┘

使用 SQLite 工具

# 用 sqlite3 命令行（需安装）
sqlite3 ~/Library/Application\ Support/com.thomas.papr/papr.db

# 常用命令
.tables                          # 列出所有表
.schema articles                 # 查看表结构
SELECT count(*) FROM articles;   # 查询文章数
SELECT * FROM feeds LIMIT 10;    # 查看 feed 列表
.quit

或使用 GUI 工具：DB Browser for SQLite (https://sqlitebrowser.org/)、TablePlus
(https://tableplus.com/)

---
6. 调试浏览器扩展

cd extension

Chrome

1. chrome://extensions → 开启"开发者模式"
2. "加载已解压的扩展程序" → 选择 extension/ 目录
3. 点击"服务工作进程"查看 background.js 日志
4. 在任意网页 F12 → Console 查看 content.js 输出

测试扩展逻辑

# 扩展的纯逻辑测试
pnpm test

---
7. Tauri 特定调试

查看 Tauri 日志

# 带完整日志启动
RUST_LOG=tauri=debug,tao=debug pnpm tauri dev

重新编译后端（不重启前端）

修改 Rust 代码后，Tauri dev 模式会自动重新编译后端，但不会重启前端。如果修改了
tauri.conf.json，需要完全重启。

查看网络请求

Tauri 内置 DevTools 的 Network 标签页可以查看前端发起的所有 HTTP 请求（包括图片加载、API
调用等）。

---
8. 常用调试命令速查

┌─────────────────────┬───────────────────────────────┐
│        场景         │             命令              │
├─────────────────────┼───────────────────────────────┤
│ 启动开发            │ pnpm tauri dev                │
├─────────────────────┼───────────────────────────────┤
│ 仅前端              │ pnpm dev                      │
├─────────────────────┼───────────────────────────────┤
│ 仅后端检查          │ cd src-tauri && cargo check   │
├─────────────────────┼───────────────────────────────┤
│ 运行前端测试        │ pnpm test                     │
├─────────────────────┼───────────────────────────────┤
│ 运行 Rust 测试      │ cd src-tauri && cargo test    │
├─────────────────────┼───────────────────────────────┤
│ 带日志启动          │ RUST_LOG=debug pnpm tauri dev │
├─────────────────────┼───────────────────────────────┤
│ TypeScript 类型检查 │ pnpm tsc                      │
├─────────────────────┼───────────────────────────────┤
│ 生产构建            │ pnpm tauri build              │
└─────────────────────┴───────────────────────────────┘

---
9. 常见问题排查

┌──────────────────┬───────────────────────────────────────────────────────────────┐
│       问题       │                           排查方向                            │
├──────────────────┼───────────────────────────────────────────────────────────────┤
│ 端口 1430 被占用 │ lsof -i :1430 或 netstat -ano | findstr 1430                  │
├──────────────────┼───────────────────────────────────────────────────────────────┤
│ Rust 编译报错    │ cd src-tauri && cargo check 查看详细错误                      │
├──────────────────┼───────────────────────────────────────────────────────────────┤
│ IPC 调用无响应   │ 检查 commands.rs 中函数名是否与 api.ts 中的 invoke() 参数一致 │
├──────────────────┼───────────────────────────────────────────────────────────────┤
│ 样式不生效       │ DevTools Elements 面板检查 CSS，注意 styles.css 是单一大文件  │
├──────────────────┼───────────────────────────────────────────────────────────────┤
│ 数据库损坏       │ 删除 papr.db 重启应用，会自动重建（丢失数据）                 │
├──────────────────┼───────────────────────────────────────────────────────────────┤
│ 国际化字符串缺失 │ 检查 src/locales/ 下的 JSON 文件是否都有对应 key              │
└──────────────────┴───────────────────────────────────────────────────────────────┘