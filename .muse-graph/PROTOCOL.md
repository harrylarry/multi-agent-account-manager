# 事实意图图谱协议（v1）

双 Muse（以后多 Muse）共享工作记忆的协议。理念借鉴 nvwabase（N1nEmAn，MIT）的事实意图图设计；本协议与一切实现均为自写，未用其代码。

定于 2026-11-22，用户拍板。试点仓库：harrylarry/multi-agent-account-manager（叠号 iOS）。

## 1. 目的

解决三个问题：跨会话、跨 Muse 的状态丢失；新任务重复踩旧坑；审核时无据可查。

图谱回答：哪些事实已确认、哪些工作在进行、哪些已完成。事实是节点，意图是边（从依赖的事实指向产出的事实），状态编码在图结构里。

## 2. 原则

- 一仓库一张图，严格隔离，不跨仓库共享。
- main 上的图只含已确认事实；PR 分支里的图更新是待确认的，合入才生效。
- Git 是唯一的同步通道，不新开通道。邮件和 Issue 是通知，不是图本身。
- 图是记录，不是聊天：只记确认的事实和工作单元，不记讨论过程。
- 谁改图，谁同步 GRAPH.md。
- 不记密钥、凭证、私人通信原文、身份/财务/健康信息。只记确认的事实，不记猜测（猜测写进 intent 描述或 hint）。

## 3. 布局

```
.muse-graph/
├── project.json          # 目标、状态、计数器
├── facts/f001.json …     # 一事实一文件
├── intents/i001.json …   # 一意图一文件
├── hints/                # 按需创建，一提示一文件
├── GRAPH.md              # 人可读摘要（用户在 GitHub 网页直接看）
└── PROTOCOL.md           # 本协议副本
```

## 4. Schema

ID 格式：事实 `fNNN`、意图 `iNNN`、提示 `hNNN`，三位补零，按 project.json 的 counters 顺序取号。

### project.json

```json
{"id": "proj", "repo": "owner/repo", "goal": "一句话维护目标",
 "status": "active", "created_at": "ISO时间",
 "counters": {"fact": 0, "intent": 0, "hint": 0}}
```

### fact（事实：已确认的观察或结果，写后不可改，只能追加纠正）

| 字段 | 必填 | 说明 |
|---|---|---|
| id | 是 | `fNNN` |
| title | 是 | 短标题，2–10 字 |
| description | 是 | 一句话，含关键证据（版本、提交、PR 号） |
| doc | 否 | 详细证据：命令、路径、输出、链接 |
| created_at | 是 | ISO 时间 |
| creator | 是 | 记录人：`wally` / `nova` / 其他 worker 名 |
| source_intent | 是 | 产出它的 intent id；直接记录的事实（用户报告、流程决定）填 `null` |
| refs | 否 | `{"issue": N, "pr": N}`，只填有的 |

### intent（意图：一个工作单元）

| 字段 | 必填 | 说明 |
|---|---|---|
| id | 是 | `iNNN` |
| title | 是 | 短标题，2–10 字 |
| from | 是 | 依赖的 fact id 列表，至少一个（任务总有依据；没有就先记一个事实） |
| to | 是 | 产出的 fact id，未结论填 `null` |
| description | 是 | 一句话：做什么、做到什么算完成 |
| doc | 否 | 详细要求：文件、根因推测、验收标准 |
| creator | 是 | 提议人，一般是 `wally` |
| status | 是 | `open`（待做）/ `claimed`（已认领在做）/ `done`（已结论） |
| worker | 是 | 认领人，未认领填 `null` |
| requires_confirmation | 是 | 危险工作填 `true`，见第 8 节 |
| confirmed_at | 是 | 用户确认时间，未确认填 `null` |
| created_at | 是 | ISO 时间 |
| concluded_at | 是 | 结论时间，未结论填 `null` |
| issue | 否 | 对应任务单 Issue 号 |

约束：`to` 非空时 status 必须为 `done`；`to` 为空时 status 为 `open` 或 `claimed`。

### hint（提示：给 commander 的留言）

`{id, content, creator, created_at}`。用户或任一方可留。

### 试点不设项目终点

维护类仓库没有“完成”那一天，goal 只是维护目标描述；完成以任务（intent）为单位，不设 terminal fact。

## 5. 角色

- **Wally（commander，唯一）**：拆任务、写 intent、审核（代码 + 图）、裁决冲突、维护 project.json 与 GRAPH.md。图更新直接提交 main（用户 2026-11-22 确认）。
- **Nova（executor）**：认领 intent、开发、写 fact（随 PR）、同步 GRAPH.md。
- **未来 executor**：取一个 worker 名，同样规则。frontier（可干的 intent 列表）就是共享工作队列。
- **用户**：提需求、confirm 门禁拍板人、验包人。

## 6. 循环（对齐先验后合流程）

1. 用户提需求 → Wally 拆任务、确认方案（老规则不变）。
2. Wally：先 recall（查 GRAPH.md 有无相似旧事实，有则引用）；写 intent 进图（直接提交 main，附 Issue 号）；开 Issue 任务单（写 intent id）；发派活邮件（写 intent id）。
3. Nova 接任务：读 GRAPH.md + intent 的 from 事实的 doc；开发；本地检查。
4. Nova 完成：写 fact（新事实 JSON，source_intent 指向 intent）+ 更新 intent（to=fact，status=done，concluded_at）+ 更新 project.json counters + 同步 GRAPH.md；全部随任务 PR 提交，PR 描述里写清图更新内容。
5. Wally 审代码：diff 对照任务单 + 审图更新（fact 是否有证据、intent 指向是否对、GRAPH.md 是否同步、id 有无冲突）。图更新不合格 → 评审意见打回，与代码问题同等处理。
6. 审过 → 手动触发分支构建 → 用户验包 → 有问题回第 3 步（Nova 修，图更新随之改）。
7. 用户确认 → 合入 main：fact 正式生效。Wally 关 Issue、删分支、报告用户。
8. 任务外的小事实（如流程决定、根因结论）：Wally 直接记 fact 提交 main，不走任务流程。

## 7. Recall（经验复用）

Wally 拆任务前、Nova 接任务后，各查一次：读 GRAPH.md，相关节点读 doc。发现重复工作时，在 intent 描述里引用旧 fact id，不重复造。

## 8. Confirm 门禁

`requires_confirmation=true` 的 intent，须用户点头后 Wally 填写 `confirmed_at`，才能执行。必须标的：删数据、改权限或密钥、发版、改 CI workflow、对外花钱的操作、其他不可逆操作。

## 9. 冲突与纠错

- 跨机 claim 靠派活邮件 + Issue，不靠图内 claim 字段（图内 worker 字段是记录，不是锁；两个 Muse 不共享文件系统，图内锁做不到实时）。一次一个任务期间不会撞。
- Nova 开 PR 前以 main 最新图为准取号；Wally 合入时若图文件冲突，让 Nova rebase 后重推（只动图文件）。
- fact 写错了：不删文件，追加一条纠正 fact（说明错在哪、正确的是什么），并在原 fact 的 doc 里追加一行指向纠正 fact。历史可追溯。
- intent 开错了：Wally 直接提交修正（改描述），或结论为“已放弃”（to 指向一个说明原因的 fact）。

## 10. GRAPH.md 格式

每次图更新必须同步更新 GRAPH.md。格式：

```markdown
# 图谱摘要（仓库名）
目标：…（来自 project.json）
状态：N 个事实，M 个 intent（X 待做 / Y 进行中 / Z 已完成），H 条提示
协议：见 PROTOCOL.md

## 前沿（可干）
- iNNN：标题（一句话）

## 最近事实（最新 8 条）
- fNNN：标题（一句话）

## 最近完成（最新 4 条）
- iNNN → fNNN：标题

## 提示
- hNNN：内容
```

无内容的部分写“（无）”，不要省略标题。

## 11. 协议维护

主本在 Wally 的 `~/workspace/dual-muse/GRAPH-PROTOCOL.md`；各仓库 `.muse-graph/PROTOCOL.md` 是副本。改协议先改主本，Wally 再推副本到各仓库（直接提交 main），并邮件通知 Nova。改动记入主本文末变更记录。

## 12. 试点与评估

先在叠号 iOS 试点；跑 3–5 个任务后评估：有用 → 做小工具（校验、自动渲染 GRAPH.md、recall 搜索）；没用 → 停，图文件留作可读记录。评估由用户定。

## 变更记录

- v1（2026-11-22）：初版。试点：harrylarry/multi-agent-account-manager。
