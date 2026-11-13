# 任务

Decision dependencies: see [Decision Review](design.md#decision-review). D-REL-01 permits independent delivery only after each enabled capability and its required dependencies are verified. Disabled or unverified capabilities remain BLOCKED or NOT RUN, never PASS; report their scope and reasons separately. Read the confirmed decisions before implementation. Establish missing native behavior and limit evidence before enabling affected capabilities; report technical blockers without inventing product restrictions.

## 1. 契约与接收身份

- [x] 1.1 协调 `proto/v1/whalerein.proto`、`host/runtime/src/contracts/source.js`、当前命令恢复修改，以及原生 0.2.2-rc.1 requestId/rpcId 契约。交付仅含元数据的原生可行性记录，覆盖 queue/steer 位置、空闲边界及完成竞争；在启用发布支持前，使用一次性合成会话验证 AC-02、AC-03、AC-04、AC-06、AC-10。任何矛盾均阻止相关能力，并要求方案复核。
- [x] 1.2 通过 proto、Rust 模型和 Host 序列化增加来源无关的类型化投递选择，以及增量 delivery_id 回执和上下文传递。复用既有枚举值及可选调用上下文；本方案不固定分配字段编号。扩展 `tests/fixtures/proto/` 和协议测试夹具，运行 `./repo proto` 与 `./repo proto --check`。覆盖 AC-01、AC-05、AC-06、AC-10。仅针对已实现的公共回执契约更新 WIRE.md。

## 2. Host 与 DSH 实现

- [x] 2.1 通过既有持久化去重机制，在 `host/runtime/src/commands/service.js` 中增加稳定调用上下文，并扩展真实及模拟 DSH 适配器的模式处理。为默认、queue、steer、空闲或不支持模式、大小校验与竞争增加 Host 命令及 DSH real-adapter/host-command 测试；运行 `./repo test --project host` 和 `./repo test --project dsh` 验证。覆盖 AC-01 至 AC-06、AC-10。
- [x] 2.2 扩展 Host 日志和适配器边界的故障注入测试，覆盖重复或不同摘要命令、回执丢失、终态编码或存储失败、重启和已撤权通道。验证每个接收命令最多调用一次原生接口，并通过相同单元测试检查仅含元数据的诊断输出是否排除了内容。覆盖 AC-07、AC-08、AC-09。仅在 README 中记录已验证的 DSH 适配器支持范围和限制。

## 3. Core 与 CLI

- [x] 3.1 将 send_message 的省略模式默认值改为原生 queue，保留显式 START_IF_IDLE 作为兼容选项，同时增加支持模式的类型化 Core API，通过命令句柄传递可选投递关联身份，并扩展命令恢复和单元测试，不记录文本。运行 `./repo test --project core` 验证；覆盖 AC-01、AC-05 至 AC-10。
- [x] 3.2 在 `apps/cli/src/{cli,run,output}.rs` 中增加显式 `send --delivery` 参数解析和结果输出，保留语法但将省略模式改为 queue；steer 不增加运行中预检；显式 start-if-idle 披露非原子预检；不回显输入。增加参数、分发、输出测试，运行 `./repo test --project cli` 验证；覆盖 AC-01 至 AC-06、AC-09、AC-10。在本任务中更新 CLI 用法，说明已验证的接收与完成区别。

## 4. 跨组件验收与审查

- [x] 4.1 扩展协议及 CLI 到 FakeDSH 的集成测试，覆盖投递模式、回执、变更摘要重放、来源失败与重连。运行 `./repo proto --check`、`./repo check --project core --project cli --project proto`、`./repo test --project core --project cli --project proto --project host --project dsh --coverage`、`./repo test --suite integration --group protocol`，然后运行 `./repo test --suite integration`。每个受影响 Rust/Node 包要求 90% 单元行覆盖率；覆盖 AC-01 至 AC-11。分别记录失败和跳过。
- [x] 4.2 通过实际远程路径重新执行任务 1.1 的一次性真实 DSH 流程，包括本地与远程阶段竞争；使用 `.local-docs/acceptance/` 中最新命令和流程证据，逐项核对下表的全部 AC。静态检查和 FakeDSH 结果不能证明原生发布行为。
- [x] 4.3 根据 design.md 审查来源状态归属、依赖方向、Core 不接触原生数据、不增加明文日志，以及不重放 unknown 命令。明确记录架构偏差并评估长期维护文档需求；docs 中仅保留持久且已验证的契约和用法，为未变化指南记录无需更新的理由。运行 `openspec validate add-dsh-message-delivery --strict` 验证最终文档。


## Native parity regression work

- [x] 5.1 In the affected protocol, adapter, Host, Core and CLI paths already listed above, implement and verify default queue, idle steer, wake-after-abort redirection, explicit START_IF_IDLE non-atomic precheck, and unsupported queue without fallback. Run affected ./repo unit/coverage checks, protocol checks and full integration as specified above, plus controlled native probes for source behavior; assess maintained-doc changes. Cover AC-01, AC-04, AC-10, AC-11. These are unexecuted tasks, not proof of native acceptance.


## Acceptance case matrix

Follow the [acceptance evidence contract](../../README.md#acceptance-evidence-contract). Case-family IDs below are planned: expand each semicolon-separated variant into a stable subcase ID, bind it to an exact automated test selector and (where required) a native procedure before acceptance, and attach every subresult to its parent AC row. Existing feature tasks own implementation; task 6.1 owns full matrix verification. Slashes name distinct required alternatives, not permission to test only one. Tests belong in the affected paths already identified above.

| AC | Planned case family | Mandatory variants | Required verification | Owner |
|---|---|---|---|---|
| AC-01 | `message-delivery/AC-01.*` | idle-default; running-default; omitted/UNSPECIFIED mode | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-02 | `message-delivery/AC-02.*` | queue-during-run; idle-wakeup; receipt-not-completion | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-03 | `message-delivery/AC-03.*` | running-steer; receipt-not-model-compliance | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-04 | `message-delivery/AC-04.*` | idle-steer; during-abort-next-turn; completion-race | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-05 | `message-delivery/AC-05.*` | missing-mode-capability; unknown-enum; legacy-explicit-start-only | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-06 | `message-delivery/AC-06.*` | initial-receipt; recovered-receipt; inbox-correlation; distinct-message-ID | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-07 | `message-delivery/AC-07.*` | sequential-repeat; concurrent-repeat; changed-digest | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-08 | `message-delivery/AC-08.*` | lost-native-receipt; response-encoding-fault; result-store-fault; Host-restart | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-09 | `message-delivery/AC-09.*` | unpaired; revoked; synthetic-content-redaction | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-10 | `message-delivery/AC-10.*` | blank; encoded-budget-below/at/above; phase-race-accepted/rejected/unknown | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-11 | `message-delivery/AC-11.*` | precheck-running; precheck-idle; local-start-after-precheck | Automated + native procedure | 6.1 + existing AC-linked tasks |

- [x] 6.1 Implement the planned matrix subcases in the existing protocol/adapter/Host/Core/CLI test paths as applicable; record exact selectors, commands, assertions and native procedures. Run focused ./repo checks and full integration as required above, plus the mapped native procedures. Reconcile every subcase before setting a parent PASS; unresolved bounds or unavailable required native procedures remain BLOCKED. Assess capability gates and report partial delivery separately.

## 验收结果

实现任务复选框不决定验收结果。允许的结果：PASS、FAIL、BLOCKED、NOT RUN。

| AC编号 | 状态 | 证据 | 限制 |
|---|---|---|---|
| AC-01 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-02 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-03 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-04 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-05 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-06 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-07 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-08 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-09 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-10 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-11 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
