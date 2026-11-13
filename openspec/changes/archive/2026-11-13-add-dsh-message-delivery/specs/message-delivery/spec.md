# 规格增量


## Purpose

让远程协作者能够明确选择指令进入会话的时机，在运行中追加下一轮工作或调整当前方向，同时保留可靠的接收确认、既有调用兼容性、稳定的关联身份，以及不会因网络或重启而重复执行的结果恢复语义。

## ADDED Requirements

### Requirement: Native queue is the default
The system SHALL use native queue for omitted CLI/Core delivery choices. Explicit START_IF_IDLE remains an optional compatibility mode; unsupported queue support SHALL fail explicitly rather than silently reverting.

#### Scenario: AC-01 Default native queue
<a id="ac-01"></a>

- **WHEN** a caller sends without selecting a delivery mode, whether the session is idle or running
- **THEN** use native queue once; it may wake idle execution or queue work for a later turn, and the receipt makes no completion claim

### Requirement: 显式排队投递
系统 SHALL 允许已授权调用方在支持该能力的会话上请求 ENQUEUE_AFTER_RUN。

#### Scenario: AC-02 下一轮投递
<a id="ac-02"></a>

- **WHEN** 运行中的会话接收下一轮指令
- **THEN** 接收回执标识该次投递，不中断当前生成；执行遵循来源队列，不报告为已经完成


### Requirement: 显式当前轮引导
The system SHALL pass supported STEER requests to the native steering path without adding a running-state precondition.

#### Scenario: AC-03 当前轮引导
<a id="ac-03"></a>

- **WHEN** 调用方在会话运行时请求引导
- **THEN** 来源的引导路径接收一次指令，结果报告接收成功，但不承诺模型立即遵从


### Requirement: Preserve idle steering
The system SHALL preserve native steering on idle Agents, including native wakeup and abort-race placement behavior, without fabricating a conflict.

#### Scenario: AC-04 Idle native steering
<a id="ac-04"></a>

- **WHEN** an authorized caller requests STEER while the Agent is idle or transitions through cancellation
- **THEN** invoke native steer and map its actual result; idle does not itself cause conflict, and native wake/next-turn redirection remains source-owned

### Requirement: 能力声明约束投递
系统 SHALL 仅公布支持的投递模式，并 SHALL 在不调用原生接口的情况下拒绝不支持或无法识别的模式。

#### Scenario: AC-05 不支持或无效的模式
<a id="ac-05"></a>

- **WHEN** 调用方请求未公布的模式，或者未知数值模式
- **THEN** 命令分别返回 unsupported_capability 或 invalid_argument，既有仅空闲发送客户端继续正常工作


### Requirement: 接收关联身份独立
系统 SHALL 为已确认接收的指令返回不透明投递身份，并 SHALL 将其与命令身份和已提交 Message 身份区分。

#### Scenario: AC-06 稳定的投递回执
<a id="ac-06"></a>

- **WHEN** 接收成功，且调用方再次读取结果
- **THEN** 返回相同投递身份；接收状态和该身份均不宣称已有已提交回复


### Requirement: 命令重试不重复接收
系统 SHALL 对每种模式保留命令去重。

#### Scenario: AC-07 重复请求与变更载荷的重放
<a id="ac-07"></a>

- **WHEN** 调用方以相同身份和载荷重复命令，或者以同一身份改用不同载荷
- **THEN** 首个请求最多接收一次；相同重复请求返回已记录结果，变更载荷则以 payload_digest_mismatch 拒绝


### Requirement: 不确定执行保持不确定
原生回执丢失或完成结果写日志失败后，系统 SHALL 保留 unknown 结果，并 SHALL 在不重新提交指令的情况下恢复状态。

#### Scenario: AC-08 回执丢失与重启
<a id="ac-08"></a>

- **WHEN** 原生调用可能已经接收指令，随后回执或日志写入丢失，包括跨重启的情况
- **THEN** 可恢复结果保持 unknown，或返回随后确认的结果；任何恢复路径均不第二次调用原生接口


### Requirement: 投递保留安全边界
系统 SHALL 要求当前有效的授权通道，并 SHALL 将指令内容排除在诊断、日志和命令恢复元数据之外。

#### Scenario: AC-09 授权与隐私
<a id="ac-09"></a>

- **WHEN** 未配对或已撤权设备尝试投递，或者在诊断中跟踪合法投递
- **THEN** 未授权投递不会到达 DSH；诊断输出不包含指令正文或传输凭据


### Requirement: 接收验证有界文本
系统 SHALL 在原生调用前拒绝空文本及超过既有协议边界的载荷，并 SHALL 在预检后运行状态发生变化时报告来源的实际接收结果。

#### Scenario: AC-10 输入限制与竞争反馈
<a id="ac-10"></a>

- **WHEN** 调用方提供纯空白或超预算输入，或者来源在预检与接收之间改变阶段
- **THEN** 无效输入返回 invalid_argument 或 payload_too_large；阶段竞争返回真实来源回执、已证明的确定性来源拒绝，或在无法确认是否接收时返回 unknown，绝不虚构生成完成

### Requirement: Explicit start-if-idle

The system SHALL offer START_IF_IDLE only as an explicit sampled-state convenience, without claiming an atomic native start-only operation.

<a id="ac-11"></a>
#### Scenario: AC-11 Explicit start-if-idle

- **WHEN** a caller explicitly chooses start-if-idle while local work may start concurrently
- **THEN** reject a precheck-observed running state before native submission; otherwise call native queue and report its actual result without claiming the precheck excluded a concurrent local start
