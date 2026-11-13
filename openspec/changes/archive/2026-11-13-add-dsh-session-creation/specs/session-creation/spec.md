# 规范增量


## Purpose

让远程用户在明确指定的主机位置创建会话，同时保留命令身份、部分变更结果以及原生来源对会话的所有权，并在不确定结果下安全恢复。

## ADDED Requirements

### Requirement: 明确目录创建

The system SHALL call native create with optional location, preset and session identity, returning the actual confirmed identity without sending a prompt. Omitted identity means new creation; supplied identity follows native reuse semantics.

<a id="ac-01"></a>
#### Scenario: AC-01 Create with an explicit directory

- **WHEN** an authorized caller supplies a valid Host directory without a session ID
- **THEN** native create mints a new identity, may ensure the directory exists, returns READY without private location/configuration in its durable receipt, and sends no prompt

<a id="ac-02"></a>
#### Scenario: AC-02 在工作区中使用预设创建

- **WHEN** 已授权客户端提供既有工作区标识和可用的 Agent 预设标识
- **THEN** 返回的新会话已绑定到该工作区并使用指定预设；省略预设时使用原生配置的默认值

### Requirement: 有效且有界的位置输入

The system SHALL reject simultaneous cwd/workspace but allow neither, using the Host native default. Native/verified protocol constraints and Host path semantics apply; the client SHALL NOT resolve a remote path locally.

<a id="ac-03"></a>
#### Scenario: AC-03 拒绝无效位置和标识

- **WHEN** 创建请求同时提供位置选择项、使用主机相对路径、目录或工作区／预设标识违反经查证的原生／现有协议限制
- **THEN** 请求在创建前以 invalid_argument 拒绝；可接受的目录语法遵循主机平台而非客户端平台

### Requirement: 授权与过期

系统 MUST 针对选定 Host 授权生命周期操作，并在原生变更前拒绝过期命令。

<a id="ac-04"></a>
#### Scenario: AC-04 拒绝未授权已撤销或过期控制

- **WHEN** 设备没有目标 Host 的有效授权、其授权已撤销，或创建命令在 Host 接纳前已过期
- **THEN** 不发生原生创建，也不向未授权调用者披露会话身份或位置信息

### Requirement: 能力发现与兼容性

系统 SHALL 仅在原生前提条件可用时宣告创建能力，并将缺失的能力信息视为不支持。

<a id="ac-05"></a>
#### Scenario: AC-05 处理旧版或不支持的主机

- **WHEN** 客户端使用缺失创建能力或创建能力为 false 的 Host，或旧 Host 无法回答主机能力查询
- **THEN** 创建以 unsupported_capability 表示不可用且不发生变更；既有 list/send/interrupt/approval 行为保持不变

### Requirement: 已知结果与部分结果

系统 SHALL 区分已证明发生在变更前的失败，以及已确认创建但工作区绑定尚未完成的结果。

<a id="ac-06"></a>
#### Scenario: AC-06 拒绝不可用的原生目标

- **WHEN** 原生来源证明所请求的工作区或预设不存在，或在变更前拒绝创建权限
- **THEN** 命令返回确定失败（`not_found`/`invalid_argument`/`unauthorized`）；若原生阶段无法证明发生在变更前，则返回保守的 `unknown`。两种情况下都不将任何会话报告为已创建，也不重试

<a id="ac-07"></a>
#### Scenario: AC-07 绑定失败后保留已创建会话

- **WHEN** 原生创建返回创建会话身份，但随后工作区绑定失败，且绑定后身份/阶段无法被原生证明
- **THEN** 已确认的生命周期结果返回保守的 `unknown`，不声称 `READY` 或完整绑定；不删除已观察到的会话，也不重试创建。只有原生明确证明身份和绑定阶段时才可使用 `READY_WITH_WARNING`

### Requirement: 持久化身份与恢复

系统 MUST 对每个设备／命令身份记录一次调用，并保留未知结果，不自动创建另一个会话。

<a id="ac-08"></a>
#### Scenario: AC-08 去重并拒绝摘要变化

- **WHEN** 同一设备使用相同身份和摘要重复创建命令，包括并发重复，或改变其摘要
- **THEN** 同摘要请求返回保留的接纳状态／结果，最多调用一次；拒绝摘要已改变的请求

<a id="ac-09"></a>
#### Scenario: AC-09 恢复丢失或不确定回执

- **WHEN** 原生调用后回复或结果存储丢失，或请求过程中 Host/Core 重启
- **THEN** 除非恢复到已持久化的确认结果，否则结果保持 unknown；恢复读取原命令，绝不提交新的原生创建；恢复的确认结果返回相同会话身份

<a id="ac-10"></a>
#### Scenario: AC-10 Independent unnamed creation

- **WHEN** different authorized commands create at the same location without a preferred session ID
- **THEN** each confirmed call returns its independently minted identity; identical location does not deduplicate different commands. Explicit named reuse follows AC-14 instead

### Requirement: 可用的远程会话与结果展示

系统 SHALL 通过常规远程会话读取暴露已确认创建的会话，并区分命令接纳与创建确认。

<a id="ac-11"></a>
#### Scenario: AC-11 发现并使用新会话

- **WHEN** 创建完成后，客户端刷新会话列表、读取返回的身份和历史，随后另行发送首条消息
- **THEN** 读取指向同一个由 DSH 拥有的会话，创建成功不代表独立首条消息的结果

<a id="ac-12"></a>
#### Scenario: AC-12 报告离线和保护隐私的结果

- **WHEN** 客户端离线请求创建，或在合成创建流程中收集人类可读／JSON 输出和运行诊断
- **THEN** 离线创建被拒绝，不排队执行变更；人类可读／JSON 输出区分接纳、确认、警告与 unknown；日志和证据不暴露私有路径、负载、凭据或原始原生错误；Host 持久化生命周期回执不包含 cwd、工作区标识或预设内容

### Requirement: Native default location

The system SHALL allow both location fields to be omitted and delegate the directory to native defaultCwd.

<a id="ac-13"></a>
#### Scenario: AC-13 Native default location

- **WHEN** an authorized caller creates without cwd or workspace
- **THEN** the source applies its configured Host default directory and preset, returns its actual identity and sends no prompt; client filesystem state does not choose the location

### Requirement: Native preferred identity and reuse

The system SHALL pass an optional preferred session ID to native create and use its returned identity without assuming reuse or minting.

<a id="ac-14"></a>
#### Scenario: AC-14 Native preferred identity and reuse

- **WHEN** the preferred identity names an eligible blank session, an ineligible started/archived session, no stored session, or an ownership/directory conflict
- **THEN** follow native reuse, fallback/new-ID, named creation or proven conflict respectively; retain preset and workspace effects, per-ID ordering and partial-attachment outcomes without guessing a new/reused disposition
