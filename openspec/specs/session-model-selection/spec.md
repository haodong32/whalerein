# session-model-selection Specification

## Purpose
让远程用户发现可用模型，明确选择会话模型与推理档位，并准确理解默认配置副作用、授权限制和不确定结果的恢复状态，避免把接受回执误认为持久化或任务完成。

## Requirements

### Requirement: 发现受支持的模型操作

系统必须（SHALL）分别声明模型目录、被动读取选择和模型修改能力。缺失的能力字段必须表示不支持，现有会话操作必须保持可用。

<a id="ac-01"></a>
#### Scenario: AC-01 旧版或不完整的执行源能力
- **WHEN** Host 省略模型能力，或只支持目录读取而不支持模型修改
- **THEN** 客户端禁用不受支持的操作，不提交修改，并继续支持现有消息发送和读取。

### Requirement: 读取可用模型目录

已获授权的在线用户必须（SHALL）获得执行源归一化的提供方/模型标识、显示名称、支持的推理档位、默认模型和修改影响范围。各提供方失败必须隔离并脱敏。

<a id="ac-02"></a>
#### Scenario: AC-02 一个提供方的目录读取失败
- **WHEN** 一个提供方失败，而另一个返回可路由的模型
- **THEN** 可用分组仍可读取；失败提供方只返回稳定可用性错误码，不包含原始错误；结果不声称目录完整且全部健康。

### Requirement: 不激活会话即可读取选择

系统必须（SHALL）区分已记录的待使用选择、最近使用选择和当前执行源默认值，且不激活 Agent。缺失的已记录选择必须明确呈现，不能伪造为会话已选择默认值。

<a id="ac-03"></a>
#### Scenario: AC-03 读取没有选择记录的冷会话
- **WHEN** 已授权用户读取一个非活动且没有模型选择记录的会话
- **THEN** 结果明确没有已记录的待使用或最近使用选择，将默认值标注为回退值，且不激活会话或发送提示。

### Requirement: Report native model-selection effects

The system SHALL follow native model-selection behavior and display its effect on the source default. The receipt SHALL identify the accepted selection, effect scope, and unconfirmed default persistence. WhaleRein SHALL NOT add an acknowledgement parameter or promise that default persistence completed. This revises AC-04 and AC-05 under confirmed decisions D-MODEL-01 and D-MODEL-02 in design.md.

<a id="ac-04"></a>
#### Scenario: AC-04 Report an accepted native selection
- **WHEN** an authorized user selects a valid model and the native controller accepts the selection
- **THEN** the source is called once, the receipt identifies the accepted model and effort, effect scope `session_and_source_default_attempt`, and default persistence `unconfirmed`; WhaleRein sends no prompt and makes no claim about task execution.

<a id="ac-05"></a>
#### Scenario: AC-05 No additional acknowledgement required
- **WHEN** an otherwise valid and authorized selection omits a separate default-effect acknowledgement
- **THEN** WhaleRein submits it to the native controller without an acknowledgement conflict and displays the native effect scope and actual outcome.

### Requirement: 由执行源验证选择

系统必须（SHALL）拒绝不可用提供方/模型路由和不受支持的推理档位，不替换为其他选择。发现目录之后，执行源仍须作出权威的接受决定。

<a id="ac-06"></a>
#### Scenario: AC-06 过期目录或不受支持的档位
- **WHEN** 先前声明的路由在调用时不可用，或请求的推理档位不受支持
- **THEN** 能证明发生在修改前的拒绝返回稳定可用性/参数错误，不回退，也不报告成功。

### Requirement: Preserve native selection acceptance conditions

The native controller SHALL decide acceptance. WhaleRein SHALL NOT reject an otherwise valid selection solely because a local observation is running, waiting for input, has a nonempty queue, or has unknown queue state. Native rejection and uncertain outcomes SHALL retain their verified error and recovery semantics. AC-07 is revised under confirmed decision D-MODEL-02 in design.md.

<a id="ac-07"></a>
#### Scenario: AC-07 Delegate acceptance to DSH
- **WHEN** an otherwise valid and authorized selection targets a running, input-waiting, queued, or incompletely observed session
- **THEN** WhaleRein invokes the native selection path without its former state preconditions and accurately maps native acceptance, proven rejection, or uncertainty; it does not cancel work or claim stronger atomicity than DSH provides.

### Requirement: 保持访问授权边界

目录读取、模型选择读取和修改必须（SHALL）使用现有 Host 授权和加密通道。未授权或已撤销的调用者不得获得执行源数据，也不得触发原生模型修改。

<a id="ac-08"></a>
#### Scenario: AC-08 已撤销设备尝试选择
- **WHEN** 已撤销设备请求目录、读取会话选择或提交模型选择
- **THEN** 在查询或修改执行源前授权即失败，且不返回提供方或会话元数据。

### Requirement: 复用模型修改的命令日志与去重

模型选择必须（SHALL）复用持久化命令身份和摘要规则。相同命令重复到达时必须返回已记录结果，不再次调用；完成状态不确定时必须保留恢复能力，不重放选择。

<a id="ac-09"></a>
#### Scenario: AC-09 重复命令与内容被改动的命令
- **WHEN** 同一命令再次到达，或相同标识被用于不同摘要
- **THEN** 相同请求返回存储的结果且不再次调用；内容变更的请求因摘要不匹配而被拒绝。

<a id="ac-10"></a>
#### Scenario: AC-10 回执或完成状态持久化丢失
- **WHEN** 原生选择可能已经发生，但无法确认回执或命令日志完成状态
- **THEN** 命令按现有恢复契约保持待定或未知；重连和结果恢复均不重复原生选择。

### Requirement: 区分回执与当前观测

成功命令回执必须（SHALL）描述该次调用。后续被动读取必须展示当前记录的选择，包括本地 DSH 更改，且不得改写原回执或暗示自动回滚。

<a id="ac-11"></a>
#### Scenario: AC-11 远程回执后发生本地选择更改
- **WHEN** 远程选择成功后，本地 DSH 再次修改模型
- **THEN** 新读取报告后来的记录，而结果恢复返回原远程回执；两者均不覆盖本地状态。

### Requirement: 离线行为只读且准确

模型修改必须（SHALL）要求执行源在线。首版不得把离线目录/模型选择缓存当作当前权威状态。

<a id="ac-12"></a>
#### Scenario: AC-12 Host 离线
- **WHEN** Host 在目录发现或模型选择期间离线
- **THEN** 用户收到离线/不可用或现有不确定命令结果；不排队延迟执行模型修改，也不把会话摘要缓存作为新目录展示。

### Requirement: 限制模型数据大小并脱敏

模型读取响应编码后必须（SHALL）不超过 经查证的原生／既有协议预算，超限时明确失败。WhaleRein 日志和诊断证据不得包含凭据、提供方原始错误、对话内容或模型调用载荷。

<a id="ac-13"></a>
#### Scenario: AC-13 提供方响应超限或包含敏感信息
- **WHEN** 目录归一化后的编码将超过 经查证的原生／既有协议预算，或提供方失败信息含敏感数据
- **THEN** 超限读取返回 `payload_too_large`；WhaleRein 用户错误、日志和证据均不包含原始错误文字，且不调用模型选择。

### Requirement: 不误报后台持久化结果

会话选择与异步默认配置持久化必须（SHALL）是独立结果。未观测到默认配置持久化，不得被解释为会话修改失败，也不得据此再次调用。

<a id="ac-14"></a>
#### Scenario: AC-14 后台默认配置持久化失败
- **WHEN** 会话模型选择成功，但执行源的后台默认配置保存失败
- **THEN** 远程回执仍表示已接受会话选择，默认值持久化为 `unconfirmed`，不自动重试或回滚。
