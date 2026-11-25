# tmux-arbor

把当前 session 的 window 和 pane 画成一棵树，贴在 tmux 侧边。侧边栏本身是一个普通 pane，焦点、鼠标、resize 和文字格式都沿用 tmux 自己的机制。用 Rust 实现，只有一个二进制 `arbor`。

```
 work          TREE   ◫  ⊟ │
────────────────────────────│
 ▾ 1:editor*                │  nvim …
 ├─ 1 nvim                  │
 ├─ 2 zsh                   │
 └─ 3 zsh                   │
 ▸ 2:server#                │
 ∙ 3:git-                   │
 ∙ 4:db!                    │
                            │
────────────────────────────│
 j k 移动 · h l 折叠        │
 ⏎ 切换 · J K 排序          │
 < > 宽度 · q 退出          │
```

## 安装

需要 tmux ≥ 3.2（状态栏 ≡ 按钮需要 ≥ 3.4；实测过 3.4 和 3.8）和 Rust 工具链（`cargo`）。

用 [TPM](https://github.com/tmux-plugins/tpm)：

```tmux
set -g @plugin 'BitBerry/tmux-arbor'
set -g mouse on
```

第一次加载时插件会在后台用 `cargo build --release` 编译，完成后自动生效；之后插件更新也会在后台重新编译，编译期间继续用旧版本。

手动安装：

```sh
git clone https://github.com/BitBerry/tmux-arbor ~/.tmux/plugins/tmux-arbor
cd ~/.tmux/plugins/tmux-arbor && cargo build --release
```

```tmux
run-shell ~/.tmux/plugins/tmux-arbor/arbor.tmux
```

## 树

采用**目录 → pane** 两层扁平结构，直接按每个 pane 所在的工作目录归组：

| 节点 | 内容 |
| --- | --- |
| 根 · 当前 session | 标题行显示 session 名与 pane / 目录数。用 prefix `(` `)` 切换 session 时整棵树替换。 |
| 枝 · 工作目录 | 默认仅展示末尾目录名（如 `tmux-arbor/`），存在同名目录时自动向上补足最短可区分父路径（`ws/app/` vs `work/app/`）；可通过 `@arbor-group-by git` 归并到 Git 根目录。折叠后若包含当前焦点 pane 或带 `#`/`!` 通知，目录行保留高亮与角标。 |
| 叶 · pane | 显示正在运行的命令（`pane_current_command`，可用 `@arbor-pane-format` 换成别的格式），右侧显示所属 window 编号与状态标记（`*` 当前焦点、`#` activity、`!` bell）；与焦点同在一个 window 的兄弟分屏用浅钢蓝底色标出。 |

侧边栏打开时，状态栏的 `status-format[0]` 换成只含 status-left / status-right 的版本（横排 window 列表交给树），并在最左边加一个 `≡` 按钮；隐藏时恢复原来的状态栏。

颜色只用一个钢蓝色：浅色底是"你在哪"，虚线是"会放到哪"，深色反白是键盘光标。浅色 / 深色会根据终端背景自动选择。

三种画法（`@arbor-style`）：

- `branches` / `indent` 简洁缩进：目录行下平铺缩进的 pane 行，最紧凑。
- `tree` 完整树线：用 `├─` / `└─` 连线把目录和 pane 串起来。
- `blocks` 线框：每个工作目录一个细线框，目录名嵌在上边框里。

被截断的目录名或 pane 文字，鼠标停在上面（或树有焦点时光标在那一行）时，会在同一行、侧边栏外侧弹出一个单行的深色标签显示全文（tmux ≥ 3.8，用浮动 pane 实现，不抢焦点）。更早的 tmux 没有浮动 pane，改为原地滚动：约 0.35 秒后开始，每列约 70ms，到头稍停再滚回来。

## 分割

标题行右侧有两个图标按钮：`◫`（水平分割）和 `⊟`（竖直分割）。鼠标停上去会提示它做什么、用哪个键，比如「水平分割 · C-x %」，前缀和键都是从你的配置读的。插件加载时用 `show -gv prefix` 和 `list-keys -T prefix` 读出前缀和所有 `split-window` 绑定（包括自定义的，比如 `bind | split-window -h -c "#{pane_current_path}"`）。

- 点按钮：用你的绑定分割工作区的当前 pane，焦点跟到新 pane。
- 树有焦点时按 prefix + 分割键：同样分割工作区的当前 pane，而不是侧边栏本身。
- 其他时候这些键完全按原样工作。

不想要按钮可以 `set -g @arbor-buttons off`。

## 宽度

宽度以列为单位，拖动、按键和配置共用一条刻度。

| 模式 | 宽度 | 显示 | 进入方式 |
| --- | --- | --- | --- |
| 完整 | 14–64 列 | 目录名、pane 命令、window 角标 | 拖动分隔线 · 聚焦后 `>` · `r` · prefix `W` |
| 窄条 | 7 列 | 按目录分隔的 window.pane 编号 | 拖到 14 列以下 · 聚焦后 `<` 到底 · `r` · prefix `W` |
| 隐藏 | 0 | 状态栏恢复 window 列表 | 拖到 4 列以下 · prefix `Tab` · 状态栏 `≡` · prefix `W` |

每次调整都写入 `@arbor-width`，所有 window 的侧边栏一起跟着变；隐藏后再打开会回到隐藏前的模式和列数。双击分隔线或聚焦后按 `=` 回到默认宽度。终端本身改变大小时侧边栏保持列数不变。

## 键盘

prefix 快捷键不抢焦点，随时可用；聚焦侧边栏后进入 vim 风格的树操作，按 `q` 或 `⏎` 把焦点还给工作区。

| prefix 之后 | 作用 |
| --- | --- |
| `Tab` | 显示 / 隐藏，记住宽度 |
| `T` | 聚焦侧边栏 |
| `W` | 完整 › 窄条 › 隐藏 循环 |
| `n` `p` `l` `1`–`9` | 原生切换 window，树同步 |
| `%` `"`（你的分割键） | 原生分割；树有焦点时分割的是工作区的 pane |
| `(` `)` | 原生切换 session，树整棵替换 |

| 聚焦时 | 作用 |
| --- | --- |
| `j` `k` `↑` `↓` | 移动光标 |
| `h` `l` `←` `→` `␣` | 折叠 / 展开目录；`h` 在 pane 上回到所属目录 |
| `⏎` | 切过去并交还焦点（在目录上为折叠 / 展开） |
| `R` `F2` | 行内重命名当前 pane（留空提交恢复默认） |
| `c` | 在当前目录新建 window |
| `1`–`9` | 直接切到第 N 个可见 pane |
| `J` `K` | 下移 / 上移目录组或同目录内的 pane |
| `<` `>` | 宽度 −2 / +2 列，到底吸附窄条 |
| `=` | 恢复默认宽度 |
| `r` | 窄条 ⇄ 完整 |
| `p` | 换到另一侧 |
| `q` `Esc` | 退出聚焦 |

## 鼠标

拖动超过一格才算拖拽，否则按单击处理。拖拽时底部显示会发生什么（"放到 api-server/ 之前"、"与 nvim 合并分屏 (w0)"……）。

| 操作 | 效果 | 对应命令 |
| --- | --- | --- |
| 单击目录行 | 折叠 / 展开目录 | 记在 session 选项 `@arbor-collapsed-dirs` |
| 悬停目录行点 `+` | 在该目录新建 window | `new-window -c` |
| 单击 pane | 切换并聚焦 | `select-pane` |
| 双击 pane / 悬停点 `✎` | 行内重命名 pane（留空恢复默认） | 记在 pane 选项 `@arbor-name` |
| 拖动分隔线 | 调宽度，吸附窄条 / 隐藏 | `resize-pane`（tmux 原生） |
| 双击分隔线 | 恢复默认宽度 | |
| 拖目录到另一个目录上半 / 下半 | 整体重排该目录的 window 顺序 | `move-window -b` / `-a` |
| 拖 pane 到另一个 window 的 pane | 合并分屏（保留各自 cwd） | `join-pane` |
| 拖 pane 到同 window 的另一个 pane | 交换位置 | `swap-pane` |
| 拖 pane 到「+ 拆成新 window」 | 拆出 | `break-pane` |
| 状态栏 `≡` | 显示 / 隐藏 | `arbor toggle` |
| 单击 `◫` / `⊟` | 分割工作区的当前 pane | 你自己的 `split-window` 绑定 |
| 悬停在 `◫` / `⊟` 上 | 提示分割方向和你的按键 | |
| 悬停在目录或被截断的名字上 | 侧边栏外侧弹出单行完整信息（旧版 tmux：原地滚动） | `new-pane -d -B none`（浮动 pane） |

## 配置

```tmux
set -g @arbor-group-by              'cwd'       # cwd | git
set -g @arbor-dir-display           'smart'     # smart（智能去重目录名）| short（~/w/...）
set -g @arbor-dir-sort              'window'    # window | alpha | active
set -g @arbor-pane-format           ''          # pane 行的文字；留空 = 正在运行的命令
set -g @arbor-hide-status-windows   on          # 打开时去掉状态栏 window 列表
set -g @arbor-status-button         on          # 状态栏最左边的 ≡ 按钮
set -g @arbor-position              'left'      # left | right
set -g @arbor-width                 '28'        # 完整模式列数，拖动后自动更新
set -g @arbor-default-width         '28'        # = 和双击分隔线回到的宽度
set -g @arbor-mode                  'full'      # full | rail | hidden
set -g @arbor-style                 'branches'  # branches/indent | tree | blocks
set -g @arbor-label-styles          on          # 是否使用格式里的 #[…] 样式
set -g @arbor-theme                 'auto'      # auto | light | dark
set -g @arbor-bg                    ''          # 侧边栏底色，如 '#e7e7ea'；留空用终端底色
set -g @arbor-lang                  'zh'        # zh | en
set -g @arbor-hints                 on          # 底部按键提示
set -g @arbor-buttons               on          # 标题行右侧的分割按钮
set -g @arbor-interval              '2'         # 可见时每隔几秒检查 pane 命令与目录；0 关闭
set -g @arbor-key-toggle            'Tab'       # 'none' 表示不绑定
set -g @arbor-key-focus             'T'
set -g @arbor-key-cycle             'W'
```

## 命令

`arbor` 也可以直接调用，比如自己绑定按键：

```
arbor toggle | focus | cycle | rail | flip
arbor resize <+N|-N|N|reset>
arbor split <h|v> <侧边栏 pane>   # 用你的绑定分割旁边的工作区 pane
arbor sync | notify
arbor uninstall      # 关掉所有侧边栏，恢复状态栏、按键和 hook
```

## 实现说明

- 每个 window 各开一个侧边栏 pane（`split-window -hbf`，右侧不加 `-b`），里面跑 `arbor sidebar`。切 window、切 session、新建 window 时由 hook 补开，宽度和位置全局一致。
- window 文字用 `#{E:window-status-format}` 在 tmux 里展开，和状态栏完全一致。
- 刷新由 hook 驱动（`session-window-changed`、`window-pane-changed`、`window-layout-changed`、`window-renamed`、`pane-exited` 等），hook 用信号通知各侧边栏。tmux 没有"pane 里的命令变了"这个事件，所以侧边栏**可见时**每 `@arbor-interval` 秒查一次 pane 命令，没变化不重画。
- 树内的点击和拖拽由 arbor 自己读鼠标事件（SGR 1006）。分隔线拖动是 tmux 原生 resize：插件包了 `MouseDrag1Border` 给 window 打上 `@arbor-drag` 标记，拖动中侧边栏只跟随鼠标、实时显示会落到的宽度，松手（`MouseDragEnd1Border` / `MouseDragEnd1Pane`，后者在没有标记时照常把事件交给 pane）时再吸附成窄条或隐藏并记住宽度。没有这个标记的宽度变化——`prefix 空格` 换布局、`rotate-window`、`swap-pane`、分屏、终端缩放——都会让侧边栏回到原宽度、贴回原侧。
- 你的 `split-window` 绑定被包了一层 `if-shell -F '#{@arbor-sidebar}'`：只有当前 pane 是侧边栏时才改由 arbor 处理，原始绑定存在 `@arbor-splits`，`arbor uninstall` 会原样恢复。
- tmux 不会告诉 pane"鼠标移走了"，所以悬停高亮和提示在最后一次鼠标移动后 3 秒消失（弹出的全文停留 5 秒，正在滚动的名字至少滚完一个来回）；如果最后一次移动是在朝工作区的那一列，0.3 秒就消失。滚轮也会把它们收起。
- 弹出的全文是一个标题为 `arbor-tip` 的浮动 pane，跑着 `arbor tip`；它放在侧边栏外面，因为浮动 pane 会截走它上方的鼠标事件。树和各种命令都会忽略它，打开它的侧边栏退出时它也跟着退出。
- 焦点在树上时 window 不会被 automatic-rename 改名成 `arbor`。
- 窗口里最后一个 shell 退出后，侧边栏也随之退出，window 正常关闭。

## 开发

```sh
cargo test
cargo build --release
```

在独立的 tmux server 上试，不影响正在用的 session：

```sh
tmux -L arbor-dev new -s work
# 在里面：
tmux run-shell "$PWD/target/release/arbor init"
```
