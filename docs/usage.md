# 使用指南

## 安装

### 图形界面(推荐)

```bash
cd macEverything
bash scripts/bundle.sh        # 构建 + 组装 + ad-hoc 签名,输出 release/macEverything.app
```

把 `release/macEverything.app` 拖进 `/Applications` 或 `~/Applications` 后双击运行。首次运行:

- 菜单栏出现图标(无窗口、无 Dock 图标——正常形态),⌥Space 即可唤起悬浮条。
- 自动注册开机自启(系统设置 → 通用 → 登录项与扩展可见);不需要可在托盘右键菜单取消。

### 命令行(可与图形界面并存)

```bash
cargo build --release

# 方式一:软链进 PATH(推荐,改代码重编译后自动生效)
ln -sf "$PWD/target/release/macEverything" ~/.cargo/bin/macEverything

# 方式二:cargo 安装(独立副本)
cargo install --path crates/cli
```

App 与命令行互不依赖:装 App 不要求命令行在 PATH,反之亦然。App 拉起引擎时优先用随包的命令行(包内 `Contents/MacOS/macEverything`),PATH 仅作兜底。

已装 App 的用户也可在设置 → 通用 →「命令行工具」一键安装(符号链接进 PATH,见「图形界面 → 命令行工具」)。

验证:`macEverything`(不带参数)打印用法。

## 图形界面

### 悬浮条

- **⌥Space** 唤起(出现在鼠标所在屏,水平居中、垂直偏上);再按 ⌥Space、Esc 或点击别处 → 隐藏。
- 每次唤起输入框清空(全新查询);输入即搜(防抖 60ms),结果行 = 文件名 + 所在目录。
- 中文输入法组字(候选窗)期间,回车/Esc 只作用于输入法,不会误触打开或隐藏。

键位:

| 键 | 行为 |
|---|---|
| ↑ / ↓ | 移动选择 |
| ↵ | 用默认应用打开(选中目录 = 访达打开);打开后悬浮条自动隐藏 |
| ⌘↵ | 在访达中显示 |
| ⌥↵ | 打开完整结果窗口并带入当前查询词 |
| Esc | 隐藏 |
| ⌥Space | 唤起 / 隐藏切换 |
| (点击别处) | 隐藏;**切换桌面 Space 导致的失焦不隐藏**(切回来仍在) |
| (双击结果行) | 用默认应用打开(目录 = 访达进入);打开后悬浮条自动隐藏 |

- 分页:首查一次取满(上限 1000 条),列表先渲染 20 行,滚到底自动加载更多(不重查、不翻页)。
- 角标:索引状态实时可见——建索引中 N(首扫)/ 补齐中(停机重放)/ 重扫中 / 离线索引 / 引擎启动中 / 索引构建中。
- 快捷键被其他应用占用时,悬浮条内常驻提示:已自动改用备选键(⌥⇧Space / ⌃⌥⇧Space)或「快捷键不可用,可从托盘唤起」。

### 隐藏项

- 设置 → **索引** →「包含隐藏项」：开启后搜索结果包含名称以 `.` 开头的文件和文件夹
  （默认关闭；升级用户首次启动保持旧行为，即包含）。
- 过滤在查询层完成，切换即时生效，不重建索引；隐藏目录内的可见文件同样不显示。
- 新装默认不包含；`macEverything exclude reset` 不会重置该开关。

### 完整结果窗口

入口(任一):

- **⌥⇧F** 直达(空词打开);
- 托盘右键菜单「打开完整窗口」;
- 悬浮条 ⌥↵(带入当前查询词)。

窗口关闭不退出 App(托盘常驻);⌥⇧F 或托盘可再次打开。

表格:

- 四列:**名称 / 大小 / 修改时间 / 路径**;列宽可拖拽;滚动虚拟化(只渲染可见行,千行结果滚动流畅)。
- 点列头排序:同列再点切换升/降,异列换键;默认排序 = 匹配质量。按大小/修改时间排序需读取全部命中的元数据,等待期状态栏显示「排序中…」,结果到达整体替换。
- 多选:单击选中;shift 范围选;⌘ 点选切换;⌘A 全选(焦点在搜索框时为全选文本);双击行 = 默认应用打开。
- 空格：表格有焦点（点击过行或 ⌘A 全选）、搜索框未聚焦、有选中项时，按空格打开/关闭快速查看；中文输入法组字期间不触发，长按只切换一次（忽略自动重复）。
- 键盘导航：搜索框输入后按 **↓** 进入表格并选中第一行（搜索框交还焦点）；表格聚焦后 **↑ / ↓** 按结果顺序移动选择，目标行自动滚入视野，长按可连续移动。选中后空格 Quick Look、右键菜单、⌘C/⌘⇧C 直接可用。
- **↵**：表格聚焦且有选中行时，用默认应用打开选中行（目录 = 访达进入）；**⌘↵** = 在访达中显示。中文输入法组字期间不触发。
- 返回搜索框：表格焦点时按 **Esc**；或已选中第一行时再按 **↑**；任意时刻按 **⌘F** 聚焦搜索框并全选当前查询词。也可直接点击搜索框继续输入。

原生右键菜单（**系统菜单**，文件与目录同形）：

| 菜单项 | 快捷键 | 行为 |
|---|---|---|
| 打开 | | 文件 = 默认应用打开；目录 = 访达进入 |
| 在访达中显示 | | 父目录中选中该项 |
| 快速查看 | 空格 | 打开/更新 Quick Look 预览；空格键开关；面板内 Esc 关闭 |
| 拷贝文件 | ⌘C | 文件 URL 写入剪贴板（粘到 Finder 复制、邮件作附件） |
| 拷贝路径 | ⌘⇧C | 复制完整路径（多选逐行） |
| 拷贝文件名 | | 复制文件名（多选逐行；根目录回退完整路径） |

- 拷贝三项平铺在一级菜单，无二级子菜单；快捷键显示在菜单项右侧。
- ⌘C / ⌘⇧C 仅在表格有焦点（点击过行或 ⌘A 全选）、搜索框未聚焦时生效；搜索框聚焦时归输入框。
- 多选：打开/在访达中显示逐项执行；快速查看以画廊打开全部选中项；拷贝文件一次性写入全部 URL；拷贝路径/拷贝文件名逐行拼接。
- 打开、在访达中显示、快速查看（打开方向）、拷贝文件、拷贝文件名会先做整批存在性校验，任一缺失则整批不执行并提示「文件不存在,索引可能待更新」；拷贝路径只复制路径文本，不校验存在性。空格关闭已打开的面板时不校验。

状态栏(底栏):

- 左段「N 项 · 匹配 X ms · 读取 Y ms」:「N 项」= 命中总数;超出单次返回上限时注明「展示前 K 条」。
- 中段索引状态:就绪 / 索引构建中(N 条)/ 索引补齐中(结果可能不全)/ 索引重扫中(结果可能不全)/ 离线索引 · 生成于 …。
- 「⚠ N 个目录未收录」徽标:点击展开缺口清单(逐目录 + 原因),与 `macEverything status` 同源。
- 排序未生效(引擎版本过旧或离线):红字提示 +「重启引擎」按钮(stop → start,新引擎接管后自动按当前列重查)。

### 菜单栏托盘

- **左键单击**:唤起 / 隐藏悬浮条。
- **右键菜单**(自上而下):引擎状态项 / ⚠ N 个目录未收录(有缺口时)/ 打开搜索 / 打开完整窗口 / 开机自启(勾选)/ 设置… / 检查更新… / 退出。
  - 引擎状态六态:构建中 / 就绪 / 补齐中 / 重扫中 / 未运行(离线索引)/ 未运行;「未运行」时点击该项可直接拉起引擎。
  - 缺口项点击 = 打开完整窗口并展开缺口清单。
  - 「检查更新…」:手动检查官网版本;发现新版本时该项变为「有新版本 vX.Y.Z — 前往下载」。
- 「退出」只退 App:引擎继续常驻,命令行与下次启动不受影响;再开 App 即恢复。

### 界面语言

设置（完整窗口标题栏齿轮或托盘「设置…」）→ 语言：跟随系统 / 简体中文 / English。默认跟随系统；切换立即生效并写入 `settings.toml`，重启后保持。

### 开机自启

- 打包形态(.app)首次启动默认注册自启;开发形态(cargo run)不注册。
- 托盘菜单「开机自启」可随时取消/恢复;取消后下次启动不再自动注册(记住你的选择)。
- 查看位置:系统设置 → 通用 → 登录项与扩展;出现「需要批准」时点允许。
- 自启对象是 App;登录后 App 自动检测并拉起引擎——重启系统后无需任何手动操作,⌥Space 即可搜。

### 发现新版本

- App 启动时静默检查一次官网版本清单;设置 → 关于 →「启动时自动检查更新」可关闭。
- 挂机期间每 24 小时自动复查一次(受同一开关控制,运行中关闭即时生效;检查进行中到点自动跳过,不叠加)。
- 设置「关于」分组的版本行显示当前版本与检查状态;发现新版本时按钮变为「前往下载」;无新版本时按钮为「检查更新」,可点击手动复查(检查中禁用)。
- 点击后仅用默认浏览器打开官网下载页,不自动下载、不自动安装。
- 每次检查仅向官网发起一次 HTTPS `GET /version.json`;不附加设备/用户标识,无遥测。User-Agent 携带应用版本号,服务端**仅按版本号聚合计数**(活跃安装分布,不记录 IP/设备);检查失败不弹窗、不影响搜索。

### 命令行工具

设置 → 通用 →「命令行工具」:把随包命令行以符号链接暴露到 PATH,免手工 `ln -sf`。

- 安装目录按 PATH 顺序取第一个允许且可写的:`/opt/homebrew/bin` → `~/.local/bin` → `~/.cargo/bin` → `/usr/local/bin`。
- 不覆盖其他来源:`cargo install`、Homebrew 或手工创建的同名条目一律显示来源并拒绝安装。
- 链接失效区分目标:指向本 App 形态的失效链接提供「修复为当前 App」;其他来源的失效链接不提供动作,也不删除。
- App 被移动导致链接失效时提供「修复」;目标卷未挂载时修复禁用,提示「目标卷未挂载」。
- 无法读取登录 shell 的 PATH、或 App 运行在 DMG/只读卷时:只显示原因与「重试」,不执行动作;DMG 形态提示「先移到 Applications 并重启 App」。开发构建(`cargo run`)不提供安装。
- 行下常驻提示:首次在终端运行可能需要完全磁盘访问权限(见「权限:完全磁盘访问」)。

### 引擎自动拉起与定位

App 启动时检测引擎(socket 连通性),不在则按下列顺序定位可执行文件并自动拉起:

1. App 可执行文件**同目录**(随包 CLI,`Contents/MacOS/macEverything`)—— 安装包自带,首选
2. `PATH` 各段查找(开发场景兜底)
3. 均未找到:悬浮条内显示可操作错误,**并列实际搜索过的全部路径**

拉起后轮询连通,≤ 3s 可用;索引构建中也可查询(结果随扫描推进增多)。重复启动 App 不会起第二份:唤起已有实例的悬浮条后,新进程自行退出。

### 保存面板目录助手

前台应用弹出保存或打开面板时(开关开启、引擎在线且已授权),面板旁自动浮出目录搜索框。

- 输入目录名 → ↑ / ↓ 选行(也可点击) → ↵ 或双击:面板跳转到该目录;浮层保留并显示原搜索内容,键盘焦点回到保存面板(可直接改文件名/保存)。要再次搜索时点击浮层即可。
- **Esc** 关闭浮层:本次面板存续内不再自动浮出,面板本身不受影响。
- 自动跳转失败、退回剪贴板时,浮层显示「路径已复制，按 ⌘⇧G 后 ⌘V 粘贴」:路径已写入剪贴板,按提示手动粘贴到系统「前往文件夹」输入框;中途取消(如关闭「前往文件夹」)不显示提示。
- 需要**「设备控制和数据访问」**权限:未授权时设置页该行显示「当前未授权，点击授权。」;在设置中开启开关会先弹实验功能确认模态(实验警告 + 权限说明),点「启用」后若尚未授权,会注册应用并打开系统设置;授权后自动生效,无需重启。托盘项在启动时按「开关开启且未授权」挂载,运行期不动态增删(授权后留到重启);设置页与托盘项均可跳转系统设置 → 隐私与安全性 → 设备控制和数据访问。
- 设置 → 实验 →「目录助手」开关(**默认关闭**);开启需确认,关闭立即生效。权限未授予时即使开关开启也不生效。
- 已知限制:自绘保存界面的应用(部分网页应用、Electron 自定义面板)无法跳转;跳转依赖「设备控制和数据访问」权限。

## 外接磁盘

本地外接磁盘(USB / 雷电 / SD 卡 / 多分区卷)默认自动纳入搜索:

- 插入 → ≤3 秒开始后台索引(不阻塞搜索),完成后卷内文件可被搜到,路径形如 `/Volumes/<卷名>/...`;
- 拔出(或推出)→ 该卷条目立即从结果消失;重插自动重扫;
- 系统卷(启动盘 / Recovery)与网络卷(SMB/NFS/AFP)不索引;
- 完整窗口或设置窗口打开期间,程序坞出现 **App 图标**:窗口被其他窗口盖住后,点一下即回到最前(⌘Tab 也可切换);两个窗口都关闭后图标自动收回;
- 设置 → 「外接磁盘」:主开关「自动索引外接磁盘」+ 按卷开关(关闭的卷被记住,重插不再自动索引);
- App 重启:上次挂着的卷延续索引(停机窗口变更自动追平),已拔的卷不残留结果。

## 五个命令

App 运行中时,以下命令同样可用(共用同一引擎与数据目录)。

```text
macEverything [--data-dir <PATH>] start
macEverything [--data-dir <PATH>] stop
macEverything [--data-dir <PATH>] query [--json] [--sort <name|size|mtime|path>] [--desc] [--include-hidden] [--] <关键词>
macEverything [--data-dir <PATH>] status [--json]
macEverything [--data-dir <PATH>] exclude add|remove|list|reset [路径]
```

参数规则:

- `--data-dir` 只能出现在子命令之前,空格与 `=` 两种写法均可(`--data-dir /tmp/mce` 等价 `--data-dir=/tmp/mce`);子命令之后出现按意外参数处理。
- `--json` 只对 `query`/`status` 有效;`--sort`/`--desc` 只对 `query` 有效,其他组合按用法错误处理。
- 退出码:**0** 成功;**1** 运行错误(连不上引擎、索引不可读、`MCE_DATA_DIR` 非法等);**2** 用法错误(未知/缺失选项、重复的 `--data-dir`/`--sort`、空关键词、相对 `--data-dir` 等),错误写 stderr,即使带 `--json` 也不输出 JSON。

### `macEverything start` — 启动引擎(后台)

```bash
$ macEverything start
引擎已在后台启动(pid 12345)。
查看进度:macEverything status;查询:macEverything query "关键词"
```

- 首次启动:全盘扫描(用户主目录 + `/Applications`),百万文件约 1-3 秒;期间即可查询(结果随扫描进度逐步完整,`status` 显示「构建中 N」)。
- 挂载中的本地外接磁盘一并纳入;运行中插入的新盘 ≤3 秒开始后台索引,拔出即清除其条目,重插自动重扫。详见「外接磁盘」节。
- 之后每次启动:**秒开**(读落盘索引)→ 后台重放停机窗口事件补齐 → 就绪。
- 单实例:重复 start 提示已在运行。
- 调试需要看实时日志:`MCE_FOREGROUND=1 macEverything start` 前台运行。

### `macEverything stop` — 停止(落盘后退出)

```bash
$ macEverything stop
已停止,最终条目数 1233836
```

响应返回时最终落盘已保证。落盘失败会如实报错(详情见 daemon.log)。

### `macEverything query "关键词"` — 查询

```bash
$ macEverything query "季度报告"
共 1 项 · 匹配 2.1 ms
     24 KB  季度报告.md  /Users/you/Documents/季度报告.md
```

- 输出:命中总数 · 引擎耗时;每条 = 大小(按需读取)· 文件名 · 完整路径(可直接复制使用)。
- 上限单轮 1000 条,超出提示;`| head` 等管道用法正常。
- **daemon 未运行时自动离线**:直读落盘索引并标注「离线索引 · 生成于 …」。

排序与机器可读输出:

```bash
macEverything query --sort mtime --desc "报告"     # 按修改时间,最新在前
macEverything query --json "*.pdf" | jq '.entries[].path'
```

- `--sort <name|size|mtime|path>` 指定排序键;`--desc` 必须与 `--sort` 同时出现,单独使用按用法错误处理。
- **`--desc` 是绝对降序**:不加它,`--sort mtime` 输出**最旧在前**——mtime 的自然方向是降序,四个键里只有它与直觉相反;`name`/`size`/`path` 不加 `--desc` 即升序。
- `--include-hidden`：本次查询强制包含隐藏项（不修改全局设置）；缺省遵循全局配置。
- 选项必须出现在关键词之前;关键词以 `-` 开头时用 `--` 分隔:`query --json -- "-foo"`。关键词开始后再出现选项按意外参数处理。
- `--json` 时 stdout 只输出一个 JSON 对象(见「--json 输出契约」),提示与日志走 stderr;人类可读输出保持不变。

### `macEverything status` — 索引状态

```bash
$ macEverything status
状态: 就绪
条目数: 约 1233836 条
索引生成: 2026-10-16 00:12:00 UTC
索引文件: /Users/you/Library/Application Support/macEverything/index.bin
权限缺口(1 个目录,未收录):
  /Users/you/Documents  (Operation not permitted (os error 1))
```

状态四态:构建中 / 就绪 / 补齐中(重放) / 重扫中(兜底)。权限缺口逐目录列出。`status --json` 输出同一信息的机器可读形态,见下节。

### `macEverything exclude add|remove|list [路径]` / `macEverything exclude reset` — 排除目录

```bash
$ macEverything exclude add /Users/you/Videos
排除目录(5 条):
  ~/Library
  node_modules
  .git
  ~/.Trash
  /Users/you/Videos
```

`add` 只接受**扫描根内绝对路径**；`remove` 接受**已有条目原文**（包括
`node_modules`、`.git` 这类相对名）。变更即时生效(引擎重扫)并写入配置文件;
daemon 未运行时此命令报错(需引擎在线)。

`reset` 无参数：清空自定义条目并恢复内置默认排除项（`~/Library`、`node_modules`、
`.git`、`~/.Trash` 及卷元数据目录 `.fseventsd`/`.Trashes`/`.Spotlight-V100`/`.DocumentRevisions-V100`），
保存配置后一次重扫生效。设置页对应「恢复默认」使用原生确认弹窗
（默认按钮为「取消」）。旧 daemon 不认识该请求，须先 `macEverything stop && macEverything start` 换用新 CLI。

## `--json` 输出契约

`query --json` / `status --json` 向 stdout 输出**一个**带 `schema_version` 的 JSON 对象,供脚本与 AI/agent 工具解析;字段只增不删,行为变更须升版本。

`query --json`:

```json
{
  "schema_version": 1,
  "total": 3,
  "elapsed_ms": 2.1,
  "stat_ms": 0.4,
  "sort_echo": "mtime",
  "desc_echo": true,
  "offline_built_at": null,
  "entries": [
    {
      "name": "季度报告.md",
      "path": "/Users/you/Documents/季度报告.md",
      "size": 24576,
      "mtime": 1760000000,
      "is_dir": false
    }
  ]
}
```

- `entries` 每条含 `name`/`path`/`size`/`mtime`/`is_dir`;`size`(字节)与 `mtime`(Unix 秒)读不到为 `null`;最多 1000 条,数量 ≤ `total`。
- `sort_echo` = 实际生效排序键(`relevance`/`name`/`size`/`mtime`/`path`);空串 = 对端旧版未回显,实际按 relevance。
- `desc_echo` = `true` 降序 / `false` 升序 / `null` 方向未验证(relevance 排序或旧 daemon 未回显)。
- `offline_built_at` = 在线为 `null`;离线直读落盘索引时为索引生成时刻(Unix 秒)。

`status --json`:

```json
{
  "schema_version": 1,
  "state": "ready",
  "entry_count": 1233836,
  "gaps": [{ "path": "/Users/you/.Trash", "reason": "Operation not permitted (os error 1)" }],
  "io_error_count": 0,
  "built_at": 1760000000,
  "index_file": "/Users/you/Library/Application Support/macEverything/index.bin",
  "offline": false
}
```

- `state` 六个取值:在线四态 `building` / `ready` / `reconciling` / `degraded`;daemon 不在时 `offline`(直读落盘索引,`offline = true`,此时 `gaps` 恒为空数组——缺口只在 daemon 内存里);`index.bin` 不存在时 `no_index`(`entry_count = 0`、`built_at = 0`,`index_file` 仍给出预期路径)。字段集在任何状态下一致。
- `index.bin` 存在但读取/校验失败(损坏、截断、无权限):不报 `no_index`,输出错误对象 `code = "index_unreadable"`,退出码 1。

`--json` 下运行失败同样向 stdout 输出一个 JSON 对象,退出码非 0:

```json
{ "schema_version": 1, "error": { "code": "warming", "message": "索引尚未就绪" } }
```

- `code`:daemon 返回的错误保留原码(如 `warming`、`version`);连接/离线路径失败为 `null`;`status` 离线索引不可读为 `index_unreadable`(query 离线路径失败为 `null`)。
- 用法错误(退出码 2)不输出 JSON:错误写 stderr,stdout 为空。

## 搜索语法

| 语法 | 语义 | 示例 |
|---|---|---|
| 关键词 | 文件名包含该子串;空格分隔 = 全部命中(AND) | `季度 报告` |
| `*` | 通配任意长度字符(含空) | `*.pdf`、`报告*` |
| `?` | 通配单个字符 | `报告?`、`invoice_??.pdf` |
| `path:` | **仅紧跟其后的一个词**匹配完整路径;其余词仍匹配文件名 | `path:Documents 发票` |
| `case:` | 本轮区分大小写(默认不区分) | `case:README` |
| `whole:` | 本轮整词匹配(默认子串) | `whole:init` |

规则细节:

- 默认大小写不敏感**覆盖整个 Unicode**(`café` 能搜到 `CAFÉ`);NFC/NFD 两种归一形态互搜。
- 排序:App 包(`.app` 目录)整体置顶;嵌套在其它应用包/插件/框架等 bundle 内的 `.app` 不置顶。组内与普通条目按「文件名前缀命中 > 词边界命中 > 子串命中;同级按名更短优先」。列头排序不受影响。
- 语法前缀须小写;`*`/`?` 在词中就是通配符(没有转义)。
- 整词边界:`_` 与字母数字同属词字符——`whole:init` 命中 `init.rs`,不命中 `__init__.py`。
- 组合示例:`path:项目 *.md`(路径含「项目」且文件名以 .md 结尾)、`path:Documents 发票 2026`(Documents 路径下文件名同时含「发票」与「2026」)。

## 权限:完全磁盘访问

macOS 的 TCC 保护会让部分目录(如 `~/Documents`、`~/Desktop`、邮件数据库)不可读,`status` 会如实列出缺口。授予方式:

1. 系统设置 → 隐私与安全性 → 完全磁盘访问权限
2. 添加 `macEverything`(软链安装时添加终端更省事)
3. `macEverything stop && macEverything start`

未授权不影响其他功能;缺口永远可见(区别于 Spotlight 的静默遗漏)。

## 数据目录

`~/Library/Application Support/macEverything/`:

| 文件 | 内容 |
|---|---|
| `index.bin` | 索引(百万条目 ~63MB;与内存布局同构,mmap 零拷贝加载) |
| `config.toml` | 排除目录列表(手工编辑后重启生效;运行时改动用 `exclude` 命令) |
| `daemon.sock` | 引擎服务套接字(0600;崩溃残留会自动清理重绑) |
| `app.sock` | App 单实例通道(0600;重复启动经它唤起已有实例的悬浮条) |
| `autostart-off` | 存在 = 你主动关过开机自启(下次启动不再默认注册);托盘重新勾选即清除 |
| `daemon.log` | 引擎日志 |

### 数据目录覆盖(`--data-dir` / `MCE_DATA_DIR`)

默认目录可用参数或环境变量覆盖,优先级 `--data-dir` > `MCE_DATA_DIR` > 默认:

```bash
macEverything --data-dir /tmp/mce start           # 空格形式
macEverything --data-dir=/tmp/mce status --json   # `=` 形式等价
export MCE_DATA_DIR="$HOME/Library/Application Support/macEverything"
```

- `--data-dir` 必须是绝对路径,且只出现在子命令前;缺失值、空值、相对路径 = 用法错误(退出码 2)。
- `MCE_DATA_DIR` 为空、仅空白或相对路径 = 运行错误(退出码 1),**不静默回落默认目录**。
- 目录权限 0700;`index.bin`/`config.toml`/`daemon.log` 0600。索引含全盘文件路径,不要把自定义目录放在共享或全局可写位置。
- `query`/`status` 不创建目录,只有 `start` 创建;`daemon.sock`/索引/日志全部跟随该目录。
- 不同目录 = 不同实例,可同时运行;**自定义目录的实例与 App 互不可见**(App 永远连默认目录)。
- `stop` 只停目标实例。共用默认目录时**不要 `stop`**——那会停掉 App 正在用的引擎;要独立生命周期就开独立实例。

外部集成(编辑器、脚本等)两种接法:

```bash
# 共享默认索引(省一次全盘扫描;不要对它 stop)
macEverything start
macEverything query --json --sort mtime --desc "发票 2026" | jq '.entries[].path'

# 独立实例(自管生命周期;会再建一份索引,磁盘/内存成本自理)
export MCE_DATA_DIR="$HOME/Library/Application Support/macEverything"
macEverything start
macEverything query --json "合同" | jq '.total'
macEverything stop
```

接入方应锁定 `schema_version`(当前 1)与协议版本,升级时回归。

## 排障

| 现象 | 处理 |
|---|---|
| 启动后不见任何窗口 | 正常形态:App 常驻菜单栏(无 Dock 图标);⌥Space 或托盘图标唤起 |
| 菜单栏找不到图标 | 系统菜单栏拥挤时图标可能被收起(刘海/溢出区);减少菜单栏常驻应用或调整排列 |
| ⌥Space 按了没反应 | 可能被其他应用占用:悬浮条内已有降级/不可用提示;托盘左键同样可唤起悬浮条 |
| 常驻内存约 136MB | 图形界面进程的技术栈基线(与窗口是否使用无关);超「<100MB」设计红线,如实记录,见[架构总览](architecture.md) |
| `macEverything: command not found` | 未安装到 PATH——见「安装」 |
| `start` 报「引擎未启动(退出码)」 | 多半已在运行(`status` 确认);否则看 `daemon.log` |
| 搜不到明知存在的文件 | ①`status` 看是否「构建中/重扫中」;②确认不在排除目录;③`status` 看权限缺口 |
| 索引文件被删/损坏 | 下次 `start` 自动重建,不需手工处理 |
| 想强制全量重建 | `macEverything stop` 后删除 `index.bin` 再 `start` |
| 占用空间 | 索引约为主目录条目数 × 55 字节;`stop` 后索引文件仍保留(这是秒开的前提) |

## 环境变量

| 变量 | 用途 |
|---|---|
| `MCE_DATA_DIR` | 覆盖数据目录(绝对路径);空/相对路径报错,不回落默认。优先级低于 `--data-dir`,见「数据目录覆盖」 |
| `MCE_ROOTS` | 覆盖扫描根(冒号分隔绝对路径);基准/隔离测试用,生产不要设 |
| `MCE_FOREGROUND` | `start` 前台运行(调试用) |
| `HOME` | 数据目录与默认扫描根随之变化(隔离测试用) |
