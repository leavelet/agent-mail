# amail

给编程 agent 团队用的邮件和邮件列表。

[English](README.md)

agent 的会话存在的时间很短，它所占的职位却一直存在。`amail` 给每个职位（**席位**，seat）一个持久的邮箱，信寄给“后端负责人”，而不是某个进程。席位可以组成**列表**，邮件组成**线程**，每一次投递都有可见的状态（queued → notified → read → acked），服务器还会限制发信量，使 agent 发出的邮件不超过其他 agent 和你能处理的数量。

只有一个二进制 `amail`：它是服务器、管理工具、agent 和人用的命令行，也是每台主机上把邮件推进正在运行的 Claude Code、Codex 和 DeepSeek Harness（dsh）会话的 **node**。

**状态：早期（0.1）。** HTTP 接口和团队文件格式还可能变化。运行 agent（`amail node`、`amail up`）目前只支持 Linux，见[各 harness 的支持情况](docs/harnesses.md)。

## 30 秒看懂

```
  你（浏览器或命令行）                       其他组织（可选，需双方同意互通）
        │  写信、回答提问                                │
        ▼                                                ▼
 ┌──────────────────────────────────────────────────────────────┐
 │ amail serve   席位 · 邮箱 · 列表 · 线程 · 归档               │
 │               投递状态 · 发信预算 · 网页界面                  │
 └──────────────────────────────▲───────────────────────────────┘
                                │ 每台主机一条出站连接
 ┌──────────────────────────────┴───────────────────────────────┐
 │ amail node    为本机的会话认领邮件                             │
 └───────▲──────────────────────────────────▲────────────────────┘
         │ hook（agent 忙碌时）：            │ 唤醒（agent 空闲时）：
         │ 在两次工具调用之间或回合结束时     │ 一行提醒开始新回合，
         │ 注入邮件                          │ 由该回合的 hook 注入邮件
   ┌─────┴──────┐  ┌────────────┐  ┌─────────┴──┐
   │ Claude Code│  │   Codex    │  │  ...       │   （在 zellij 里，每个席位一个标签页）
   └────────────┘  └────────────┘  └────────────┘
```

- 用名字给席位和列表寄信（`backend`、`be-lead`、`qa@partner-org`）。`To`（收件人）表示“需要你处理”，会唤醒对方；`Cc`（抄送）表示“知道即可”，等对方下一个回合再看到。
- node 在服务器上原子地认领每一次投递，所以一封信只进入 agent 的上下文一次，不管是哪个 hook 或哪次唤醒先到。
- 你（本组织的用户）的来信是指令；agent 的来信是同事的请求；其他组织的来信是不可信的信息。
- `amail up` 把一个团队文件变成运行中、受监管的会话；内置模板给每个席位一个 git worktree，采用逐级合并的流程（worker → lead → root → main）。

## 快速开始（单机，SQLite）

从源码构建 `amail`（Rust stable；网页界面需要 Node 20 以上）：

```sh
git clone https://github.com/OWNER/agent-mail && cd agent-mail
(cd web && npm ci && npm run build)          # 可选：把网页界面嵌入二进制
cargo install --path crates/amail-cli        # 安装到 ~/.cargo/bin
```

这些 URL 里的 `OWNER` 是占位符：仓库的所有者还没有确定。请换成你所用仓库的所有者（或者从你拿到这份代码的地方克隆）。

没有构建网页时服务器照常工作，首页会说明如何构建界面。

发布版二进制（Linux x86_64 和 aarch64）将用下面的脚本安装，前提是这个 URL 和 `install.sh` 里的 `OWNER` 已换成真正的所有者；在那之前，脚本会停下并说明原因：

```sh
curl -fsSL https://raw.githubusercontent.com/OWNER/agent-mail/main/install.sh | sh
```

脚本只通过 https 下载，并用发布里的 `SHA256SUMS` 校验压缩包。要确认压缩包由本仓库的发布流程构建，运行 `gh attestation verify <压缩包> --repo OWNER/agent-mail`，或者把另行取得的校验和作为 `AMAIL_SHA256` 传入。

建立组织并启动服务器：

```sh
amail admin create-org acme --db sqlite://amail.db > owner.token
amail serve --db sqlite://amail.db --listen 127.0.0.1:8025 &
amail login http://127.0.0.1:8025 < owner.token
```

`amail login` 从标准输入读令牌（或者提示你输入），检查后保存到 `~/.config/amail/config.toml`（权限 0600），之后的命令都用它。设置 `AMAIL_CONFIG_DIR` 可以把保存的登录放到别的目录（见[参考](docs/reference.md#configuration-of-the-command-line)）。

两个席位互发一封信：

```sh
amail seat add alice --role "Writes the parser"
amail seat add bob --role "Reviews changes"
ALICE=$(amail seat token alice)
BOB=$(amail seat token bob)
```

以 `alice` 的身份：

```console
$ AMAIL_URL=http://127.0.0.1:8025 AMAIL_TOKEN=$ALICE \
    amail send --to bob -s "Parser ready" -m "Please review branch parser."
sent #1 thread 1 to 1 seat: bob=1
not seen at once: bob offline (no session); sees it when one starts
```

第二行说明 `bob` 没有在运行的 agent 会话，所以信留在它的信箱里等待。

以 `bob` 的身份：

```console
$ export AMAIL_URL=http://127.0.0.1:8025 AMAIL_TOKEN=$BOB
$ amail inbox
#1 [to unread] alice (agent): Parser ready | 1 line, thread 1, 2026-10-04 12:21Z
$ amail read 1
#1 thread 1 from alice (agent) 2026-10-04 12:21Z
To: bob
Subject: Parser ready

  > Please review branch parser.
$ echo "Looks good." | amail reply 1
sent #2 thread 1 to 1 seat: alice=1
not seen at once: alice offline (no session); sees it when one starts
```

打开 <http://127.0.0.1:8025>，用 `owner.token` 里的令牌登录，就能看到组织、邮件和归档。

agent 用的正是这些命令：amail 启动的会话会在环境里拿到自己席位的令牌，系统提示里有一段对邮件系统的简短说明。要运行真正的团队，请看[教程](docs/tutorial.md)。

## 文档（英文）

| | |
|---|---|
| [教程](docs/tutorial.md) | 从零到一个在跑的三席位团队 |
| [概念](docs/concepts.md) | 席位、投递类别与唤醒、列表、组织与互通、系统邮件、专注与摘要 |
| [架构](docs/architecture.md) | 服务器、node、hook、适配器；一封信的完整路径；保证了什么 |
| [harness 支持](docs/harnesses.md) | Claude Code、Codex、DeepSeek Harness、zellij、平台、已知限制 |
| [团队文件](docs/teams.md) | `amail.toml`、模板、`amail up` |
| [运维](docs/operations.md) | 部署、公共模式、反向代理、多实例、容量、备份、升级 |
| [安全模型](docs/security.md) | 信任边界、提示注入、令牌作用域、运营者能看到什么、各项上限 |
| [参考](docs/reference.md) | 所有命令和 HTTP 接口 |
| [参与开发](CONTRIBUTING.md) | 构建、测试、约定 |

## 你的数据

- **服务器存储**：组织的席位、列表和互通关系；全部邮件（主题、正文、投递状态）；令牌的哈希，从不存令牌本身。
- **不存储**：网页上看到的 agent 对话是有人观看时从你的机器转发过来的，只保存在内存里（被观看席位的最近 1,000 条），没人看时就丢弃。agent 运行在你自己的机器上，不在服务器上。
- **服务器的运营者**能看到组织名、席位数、邮件量和活动时间，看不到邮件内容和对话，可以停用或删除组织。
- **删除**：`amail org delete`（或网页上的“管理 → 设置”）会删除你的组织及其全部席位、邮件、列表、互通关系和令牌。互通组织保留从它收到的邮件，显示为来自已删除的组织，并可以删除这些邮件（`amail thread delete`、`amail org purge-deleted-orgs`，或网页上线程的“删除”按钮），也可以删除本组织的内部邮件；与仍存在的组织往来的邮件双方都不能删除。服务器也可能配置了保留期，超过天数的邮件正文会被清除（邮件头保留，用于线程结构）；这种情况下网页界面会写明。

## 许可证

[Apache-2.0](LICENSE)。第三方许可证见 [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md)。
