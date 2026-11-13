# 设计方案

## Decision Review

Confirmed rows record only the choices listed below. Other draft choices still need evidence or review.

| ID | Decision / recommendation | Alternatives / tradeoffs | Status | Basis | Affected acceptance |
|---|---|---|---|---|---|
| D-REL-01 | Deliver verified capabilities independently; keep unverified capabilities disabled and report them. Dependencies still apply. | Deliver all seven together, or wait for all native probes. | CONFIRMED | User reply in this chat on 2026-11-11. This confirms delivery policy, not runtime success or blanket approval of the plan. | All ACs |
| D-CREATE-01 | Support omitted location (Host default), cwd or workspace, and optional native session-ID reuse. Return the actual identity; never label every accepted create as a newly minted Session. | The previous mandatory-location/new-only scope was narrower than DSH. | CONFIRMED | Explicit user choice; native create(), createSession() and claim() inspected. | AC-01–AC-03, AC-07, AC-10, AC-13, AC-14 |
| D-LIMITS-01 | Reuse verified DSH and existing protocol limits first. Investigate and separately justify any necessary additional bound; seek review when it changes observable behavior. | Earlier draft numbers were not established native constraints. | CONFIRMED | Explicit user choice in this chat; exact source limits still need evidence. | AC-03 |

## 上下文

动机见[提案](proposal.md)，可观察行为见[规范](specs/session-creation/spec.md)。已检查的 `host/runtime/src/contracts/source.js` 仅提供会话范围执行；`host/runtime/src/commands/service.js:commandOperation` 仅接纳发送／中断／审批控制。既有命令日志在调用前持久化接纳记录，并保留未知结果。`core/src/api/core.rs` 负责带类型控制及不确定意图的持久化恢复，CLI 只负责解析和格式化。`proto/v1/whalerein.proto:377-418` 尚无生命周期协议消息。

本机安装的 DSH 0.2.2-rc.1 `dsh-api-session-controller/lib/index.js:666-697` accepts optional mutually exclusive cwd/workspace, uses defaultCwd when neither is supplied, and accepts an optional preset and preferred session ID. 省略原生 sessionId 可避免有意复用指定名称的空白会话。服务可能先创建会话，再抛出带结构化 sessionId 的 `session/workspace-attach-failed`。目前仅检查了代码，未对真实 DSH 执行。

## 目标与非目标

**目标：** 增加最小的主机范围生命周期接口，维持与既有控制相同的授权和执行日志保证，并在部分完成时保留已确认身份。

**非目标：** 不引入来源无关的文件系统／工作区注册表、创建并发送的组合操作、明文位置／提示词持久化存储、重命名，或 Rust Core 的额外注入边界。

## 设计决策

### 架构评审

| 层 | 职责与依赖方向 |
|---|---|
| CLI | 解析 `session-create --host HOST [--cwd PATH \| --workspace ID] [--preset ID] [--session-id ID]`，格式化带类型生命周期回执或待恢复身份。不得使用客户端平台文件系统 API 解释原生路径。 |
| Rust Core | 查询 Host 能力，通过既有加密请求流水线提交带类型创建控制。在 HTTP 提交前保留既有持久化不确定意图身份；在 `api/types.rs` 暴露带类型生命周期结果。 |
| Access | 沿用授权、请求摘要／类型校验和不透明转发。不解释会话／位置，不新增路由或数据库会话权威。 |
| 共享 Host | 新增 `host_capabilities` 查询、与来源无关且包含创建支持和输入限制的 HostCapabilities。新增 `SourceAdapter.describeHostCapabilities()` 和 `executeHost(command)` 支持主机范围控制。`execute(sessionId, command)` 仍为会话范围；两者共用既有命令接纳／日志／结果编码路径。 |
| DSH 适配器 | 检测服务，执行原生主机路径及标识约束，将创建／错误回执映射为归一化生命周期结果。所有 DSH API 访问仍留在此处。 |
| DSH | 拥有会话创建、工作区成员关系、实际预设和会话历史。 |

主机范围方法优于伪造空或虚拟会话标识。其他提议变更扩展 HostCapabilities，不另造发现协议。实现时在协调并行变更后分配 protobuf 编号；本方案不预留编号。

### 输入与回执契约

Location options are mutually exclusive, not required: omitting both delegates to native defaultCwd. Optional session-id is the native preference for blank reuse or creation, not a guarantee of the returned identity. Native createMinted may create the directory recursively; disclose that filesystem effect rather than requiring a pre-existing directory. Path and identifier lengths follow verified native and existing protocol constraints under D-LIMITS-01; remove the proposed 4096/512-byte product limits and reject blank identifiers. Record the constraint source and boundary fixtures before enforcing any additional bound. Host 在调用前按自身平台验证绝对路径语法；实际位置及预设的授权／可用性由 DSH 检查。CLI/Core 不对远程路径调用 `resolve()`。省略可选预设时委托 DSH 默认配置。创建不发送提示词，也不保证模型能执行提示词。

增加归一化生命周期结果，包含会话身份、请求操作、创建处置状态及可选稳定警告码；编码后的持久化回执不包含 cwd、工作区标识和预设内容。`READY` means the native create/reuse operation returned an actual session identity; it does not prove a new Session was minted.`READY_WITH_WARNING` means an identity is confirmed but workspace attachment failed after native create/reuse; do not retry or compensate by deleting it.两者均为带明确领域处置状态的持久化确认命令结果，不能据此声称绑定成功。调用前校验失败时不含已创建身份。避免记录路径／预设内容或复制原始原生错误。经过验证的结构化部分回执是执行证据，不能当作普通抛出的确定失败。回执丢失后不得按标题或路径查询来猜测会话。

### 所有权、并发与失败

沿用 `(device_id, command_id, digest)` 命令去重。重放返回原接纳状态／结果，包括部分处置状态；Distinct commands without a requested session ID mint independently. With a requested ID, native blank-session reuse, per-ID ordering, placement/ownership checks and fallback-to-new behavior apply; transport command deduplication is a separate concern.原生会话 ID 保持不透明。与既有 CommandService 一样，调用后的结果编码／日志失败保留 `unknown`。Core 通过新通道读取原命令结果进行恢复。禁止补偿删除、以新命令 ID 自动重新提交，或根据位置推断重试。

Host SQLite 以明文存储编码后的 ResponsePayload 结果，因此生命周期回执 MUST 仅包含会话身份、操作、处置状态和稳定警告／错误码，MUST NOT 包含 cwd、工作区标识、预设或其他私有位置／配置内容。确认后通过独立权威读取获取实际位置／配置；Core 仅可经既有加密缓存保存常规摘要。摘要读取失败不能改变已确认的变更结果。Host 状态只保存这些有界回执字段，不建立第二份对话日志。确认后由常规 DSH list/get/history 填充 Core 会话投影。摘要刷新失败不能追溯地将已确认创建变成未创建，也不能触发再次创建。

### 兼容性与发布

增加带类型的 v1 请求／响应负载和能力字段，不改变既有路由／主题，也不复用字段编号。Host 与 Core/CLI 同步升级，并验证协议夹具。缺失能力字段视为 false；旧 Host 对能力查询返回的 `invalid_argument` 归一化为不支持，不通过变更试探能力。适配器缺少前提服务时禁用创建。既有命令保持不变。回滚禁用能力，不删除会话、授权或命令恢复记录。

## 验收评审

| AC 编号 | 场景与预期结果 | 验证方法 | 来源场景 |
|---|---|---|---|
| AC-01 | Explicit directory without a requested session ID: confirm the actual new identity, omit private configuration from the persisted receipt, and send no prompt | 适配器／Host 单元测试与隔离全链路集成；真实 DSH 合成配置检查 | [AC-01](specs/session-creation/spec.md#ac-01) |
| AC-02 | 在工作区中使用预设创建：返回的新会话已绑定到该工作区并使用指定预设；省略预设时使用原生配置的默认值 | 适配器／Host 单元测试与隔离全链路集成；真实 DSH 合成配置检查 | [AC-02](specs/session-creation/spec.md#ac-02) |
| AC-03 | 拒绝无效位置和标识：请求在创建前以 invalid_argument 拒绝；可接受的目录语法遵循主机平台而非客户端平台 | 适配器／Host 单元测试与隔离全链路集成 | [AC-03](specs/session-creation/spec.md#ac-03) |
| AC-04 | 拒绝未授权已撤销或过期控制：不发生原生创建，也不向未授权调用者披露会话身份或位置信息 | 适配器／Host 单元测试与隔离全链路集成 | [AC-04](specs/session-creation/spec.md#ac-04) |
| AC-05 | 处理旧版或不支持的主机：创建以 unsupported_capability 表示不可用且不发生变更；既有 list/send/interrupt/approval 行为保持不变 | 适配器／Host 单元测试与隔离全链路集成 | [AC-05](specs/session-creation/spec.md#ac-05) |
| AC-06 | 拒绝不可用的原生目标：命令返回稳定的确定失败，不将任何会话报告为已创建 | 适配器／Host 单元测试与隔离全链路集成 | [AC-06](specs/session-creation/spec.md#ac-06) |
| AC-07 | 绑定失败后保留已创建会话：已确认的生命周期结果包含该身份、READY_WITH_WARNING 和绑定失败详情；不报告完整绑定、不删除会话，也不重试创建 | 适配器／Host 单元测试与隔离全链路集成；真实 DSH 合成配置检查 | [AC-07](specs/session-creation/spec.md#ac-07) |
| AC-08 | 去重并拒绝摘要变化：同摘要请求返回保留的接纳状态／结果，最多调用一次；拒绝摘要已改变的请求 | 适配器／Host 单元测试与隔离全链路集成 | [AC-08](specs/session-creation/spec.md#ac-08) |
| AC-09 | 恢复丢失或不确定回执：除非恢复到已持久化的确认结果，否则结果保持 unknown；恢复读取原命令，绝不提交新的原生创建；恢复的确认结果返回相同会话身份 | 日志／编码故障注入、Core 恢复与重启集成 | [AC-09](specs/session-creation/spec.md#ac-09) |
| AC-10 | Without a requested session ID, distinct commands mint independently; requests explicitly naming one ID follow native reuse/ordering instead | 适配器／Host 单元测试与隔离全链路集成 | [AC-10](specs/session-creation/spec.md#ac-10) |
| AC-11 | 发现并使用新会话：读取指向同一个由 DSH 拥有的会话，创建成功不代表独立首条消息的结果 | 适配器／Host 单元测试与隔离全链路集成 | [AC-11](specs/session-creation/spec.md#ac-11) |
| AC-12 | 报告离线和保护隐私的结果：离线创建被拒绝，不排队执行变更；人类可读／JSON 输出区分接纳、确认、警告与 unknown；日志和证据不暴露私有路径、负载、凭据或原始原生错误；Host 持久化生命周期回执不包含 cwd、工作区标识或预设内容 | 适配器／Host 单元测试与隔离全链路集成 | [AC-12](specs/session-creation/spec.md#ac-12) |
| AC-13 | Native default location: the source applies its configured Host default directory and preset, returns its actual identity and sends no prompt; client filesystem state does not choose the location | Native default directory and CLI/adapter integration tests | [Scenario](specs/session-creation/spec.md#ac-13) |
| AC-14 | Native preferred identity and reuse: follow native reuse, fallback/new-ID, named creation or proven conflict respectively; retain preset and workspace effects, per-ID ordering and partial-attachment outcomes without guessing a new/reused disposition | Native blank/started/archived/conflicting identity fixtures plus command recovery | [Scenario](specs/session-creation/spec.md#ac-14) |


## 风险与权衡

- 原生创建可能成功后工作区绑定才失败 → 保留结构化会话身份和明确警告；绝不重试或声称已回滚。
- 当前安装服务可能无法安全提取原生错误详情 → 发布启用能力前，在隔离配置验证真实结构化错误封装；可能发生变更但没有可靠结果时返回 unknown。
- 调用者路径可能只在主机平台有效 → 在适配器解释并拒绝相对路径，不访问客户端文件系统。
- 创建回执不证明后续生成 → 首条消息独立控制并具有独立结果。
- 并行变更共享协议／Core 文件 → 由首个消费者实现共享基础，再分别接入创建／分叉／模型操作，协调增量字段，不创建独立注册表。

## 迁移计划

1. 协调当前进行中的协议／Core 恢复变更，实现增量夹具和主机范围接口。
2. 实现模拟适配器、真实适配器、Core 和 CLI；缺少服务时保持能力禁用。
3. 在隔离 DSH 0.2.2-rc.1 配置中验证完整协议／恢复行为以及原生创建／列表／重载／工作区警告场景。这些是后续发布证据，并非当前已经验证的声明。
4. 仅为已验证且兼容的运行时宣告能力。回滚保留既有命令结果和已创建的 DSH 会话。

## Agent 执行说明

使用既有夹具接口：`host/runtime/test/commands.test.js`、`source-contract.test.js`、DSH `real-adapter.test.js`、Core 命令恢复测试和 CLI 解析／派发测试。原生检查必须证明显式目录语法、预设／默认选择、创建后工作区失败封装、独立请求获得新身份，以及重启后的会话可见性。原生行为若与验收行矛盾，保持能力不可用并报告冲突，不能静默弱化验收行。

## 验证约定

使用已注册的 `host`、`dsh`、`core`、`cli`、`proto` 和 `access` 项目 ID。实现期间运行 `./repo check --project core`、`./repo check --project cli`，并仅在 proto/access 发生变化时运行其支持的检查。Host 和 DSH 仅支持测试操作：使用 `./repo test --project host --coverage` 和 `./repo test --project dsh --coverage`，不得对这两个 Node 项目调用 check/build。也要为发生变化的每个适用 Rust/Go 包运行覆盖率测试；每个适用包／crate 的实测覆盖率至少为 90%。预期生成变更使用 `./repo proto`，并运行 `./repo proto --check`、`./repo test --suite integration --group protocol`；这一跨组件恢复变更还需运行 `./repo test --suite integration`。原生 DSH 检查使用隔离配置和合成内容，证据只在 `.local-docs/acceptance/` 记录安全标识／计数。隔离测试通过不证明原生持久化或生产部署行为。

### Shared contract dependency

Shared capability, invocation-identity, lifecycle and input-state types are implemented once by the first consumer. A dependency on these types is not a requirement to enable another entire feature first. Respect actual protocol/source prerequisites and independent capability verification.

## Native parity review evidence

Controller create()/AgentHostController.createSession()/claim() implement defaultCwd, optional preferred identity, native blank reuse and fallback IDs, changed preset application, recursive mkdir, and workspace attachment after create/reuse. The dsh-session header validator requires an absolute cwd. READY is normalized confirmed identity, not proof of minting.

Evidence is from the locally installed DSH 0.2.2-rc.1 packages, reviewed read-only. Native runtime/integration acceptance has not run. Existing transport authorization, command deduplication, encryption, privacy and uncertain-outcome recovery remain intentional remote-adapter boundaries.

## Capability acceptance gates

These are logical operation gates, not mandatory new wire flags. Follow the [evidence contract](../../README.md#acceptance-evidence-contract): all listed AC variants need current evidence, and source behavior needs the mapped native procedures. A flag covering several operations is enabled only when all its operation gates pass. Negative unsupported-capability tests passing do not enable an unverified operation. Existing shipped operations remain governed by their existing support; report any regression separately.

| Operation | Required ACs (all mandatory variants) | Required native evidence subset |
|---|---|---|
| Native create/reuse contract | AC-01–AC-14 | AC-01, AC-02, AC-06, AC-07, AC-10, AC-11, AC-13, AC-14 |

Record each operation as enabled or disabled, its gate evidence, blockers and tested native version in the delivery summary. Keep any disabled path unavailable and retain FAIL/BLOCKED/NOT RUN results. A partial rollout is not full acceptance. The [combined workflow cases](../add-dsh-session-creation/design.md#combined-workflow-acceptance) qualify the integrated feature set separately and do not block independent delivery of verified operations.

## Combined workflow acceptance

This is the single definition of group cases GX-01–GX-05 for all seven plans.
They test integration between already-defined behaviors and introduce no new
product scope. Per-operation capability gates remain independent. A combined
case requires the stated operations; unavailable required positive variants are
BLOCKED, not silently skipped or counted as passed. GX-05 may pass its negative
partial-rollout checks while the positive combined workflows remain blocked.

Run deterministic/fault variants through the real CLI-to-FakeDSH encrypted
integration path, then run native-semantic variants against an isolated DSH
configuration through the same remote boundary. Direct adapter calls alone do
not establish end-to-end readiness. Use synthetic content and record only safe
metadata, IDs, version/configuration and assertion results. Apply the common
[acceptance evidence contract](../../README.md#acceptance-evidence-contract).

| Case | Required operations | WHEN | THEN | Linked feature ACs |
|---|---|---|---|---|
| GX-01: Create, select, send, wait and answer | creation, model selection, queue sending, wait, question discovery/answer | create using default/explicit placement and repeat the eligible reuse variant; select a model, send a synthetic queued prompt, wait for input, explicitly answer an open or continued batch, then wait again | the same Host/session identity flows end-to-end; each result retains its own acceptance/settlement meaning; wait reports native stopping states, question answers are explicit and no step auto-approves input. Test open and continued branches separately | session-creation/AC-01, AC-02, AC-13, AC-14; session-model-selection/AC-04, AC-11, AC-14; message-delivery/AC-01, AC-02; session-wait/AC-01–AC-04, AC-11; user-questions/AC-01, AC-03, AC-04, AC-12–AC-14 |
| GX-02: Fork and independently continue | creation, fork, model selection, sending, reads/subscriptions | prepare a completed parent turn, observe native default-model/preset state, fork its latest complete prefix and explicitly continue the child while the parent remains available | the child has its own identity/history/cache/Attempt state and no automatic prompt; cwd is shared without worktree isolation; its model/preset follows native resolution rather than a forced parent copy; child updates do not replace the parent view | session-creation/AC-11; session-fork/AC-01, AC-02, AC-05, AC-10, AC-11; session-model-selection/AC-11, AC-14; message-delivery/AC-01, AC-06 |
| GX-03: Queue editing, interruption and native waiting | queue read/edit/remove/promote, interruption, sending, wait | enqueue synthetic work, edit and explicitly remove eligible items, exercise queued-item promotion separately from direct idle steering, interrupt active work, then inspect fresh queue state and wait | edits preserve native replacement semantics and IDs; removing one item preserves unrelated work; interruption does not implicitly clear inbox work; remaining state follows fresh native observation, and native idle is never described as every queued task having succeeded | pending-queue/AC-01–AC-06, AC-08, AC-12; message-delivery/AC-03, AC-04; session-wait/AC-02, AC-05, AC-11 |
| GX-04: Recover accepted controls without replay | all enabled mutating operations plus waiting and result recovery | lose the response or inject result-store failure after each enabled mutation, reconnect or restart Host/Core, recover its command result, and receive an old wait result after the channel changes | no native mutation is submitted twice; confirmed results retain their identities, unresolved outcomes remain unknown, fresh reads do not invent certainty or undo prior acceptance, and stale waits are rejected/cleaned without extending the user deadline | message-delivery/AC-07, AC-08; session-creation/AC-08, AC-09; session-fork/AC-08, AC-09; pending-queue/AC-07, AC-12; session-model-selection/AC-09, AC-10; user-questions/AC-07, AC-09; session-wait/AC-05, AC-08, AC-12 |
| GX-05: Partial rollout and disable lifecycle | capability discovery, every enabled operation, existing baseline operations | run against absent/partial capabilities and an unverified open-answer path, then withdraw a new capability while accepted controls and scoped observations exist | unsupported/unverified paths fail explicitly without silent fallback or local UI interception; supported baseline and independently verified paths remain usable; accepted control receipts are not rewritten or replayed, observations clean up, and the report distinguishes partial delivery from full acceptance | message-delivery/AC-05; session-creation/AC-05; session-fork/AC-06; pending-queue/AC-09, AC-10, AC-12; session-model-selection/AC-01; user-questions/AC-10, AC-14; session-wait/AC-05, AC-09, AC-12 |

Group acceptance requires all required variants of GX-01–GX-05 at the same tested
revision/native configuration. Changes to shared protocol, invocation identity,
source state or lifecycle results require re-running affected group cases.
A delivery claiming only a subset lists its passing feature gates and outstanding
group cases explicitly; it does not claim the complete workflow set is accepted.
