# 实施任务

Decision dependencies: see [Decision Review](design.md#decision-review). D-REL-01 permits independent delivery only after each enabled capability and its required dependencies are verified. Disabled or unverified capabilities remain BLOCKED or NOT RUN, never PASS; report their scope and reasons separately. Read the confirmed decisions before implementation. Establish missing native behavior and limit evidence before enabling affected capabilities; report technical blockers without inventing product restrictions.

Prerequisite: implement the shared capability/lifecycle contract once with the first consumer. The creation operation itself need not be implemented or enabled before fork.不要重复建立基础。实施复选框与验收结果彼此独立。

## 1. 带类型的分叉契约

- [x] 1.1 将创建方案的能力／回执字段与当前协议改动协调，增加会话范围的 `fork_session` 和分叉支持标志，不增加原生切点字段；运行 `./repo proto` 重新生成，增加响应／旧主机／能力夹具；验证 `./repo proto --check` 和协议夹具测试（AC-01、AC-04、AC-06、AC-07、AC-11）。
- [x] 1.2 扩展 `host/runtime/src/commands/service.js`，通过既有 `execute(sessionId, command)` 和持久化日志路由；为同摘要并发重复、摘要改变、过期、授权撤销／错误 Host 和独立控制增加原生调用计数；验证 `./repo test --project host` 及相关 Access 授权单元场景（AC-06、AC-08–AC-10）。
- [x] 1.3 在 WIRE/Harness 契约中记录已实现的分叉请求／回执和能力语义；通过夹具证明无 atSeq／原生游标运算，也无替代传输路由（AC-01、AC-04、AC-06–AC-09）。

## 2. 适配器分叉语义

- [x] 2.1 实现真实／模拟适配器分叉，使用源身份并省略 atSeq，稳定映射变更前的来源缺失／无完整轮次，并检查原生服务能力；增加空闲／冷／运行中来源夹具场景，验证 `./repo test --project dsh`（AC-01–AC-04、AC-06）。
- [x] 2.2 归一化已确认子会话和结构化工作区警告回执，明文 Host 日志只保存安全身份／处置状态／错误码；确认后另行读取实际目录；保留父调用身份，所有未证明的变更后失败保持 unknown；增加错误／回执／持久化日志故障注入测试，验证已存生命周期负载不含合成路径／工作区／预设哨兵，并运行 `./repo test --project dsh` 和 `./repo test --project host`（AC-07–AC-10、AC-12）。
- [x] 2.3 增加投影／测试框架测试，证明对话上下文继承、后续排队任务不被自动执行、源未改变，以及子历史／缓存命名空间独立；验证 `./repo test --project dsh` 和 Host 订阅测试，仅按已确认的持久边界变化更新 DSH 架构（AC-01、AC-02、AC-05、AC-11）。

## 3. Core 与 CLI

- [x] 3.1 增加归一化 Core 分叉 API／结果处理，以及 Host／会话身份下的独立子基线；扩展 `core/src/api/core/command_recovery_tests.rs` 和缓存／订阅测试，覆盖回执丢失、重启、不同父／子 Messages、共享适配器 epoch 字符串下仍独立的会话范围 Attempt 状态，以及源订阅连续性；验证 `./repo test --project core`（AC-01、AC-04–AC-10、AC-12）。
- [x] 3.2 增加 CLI `session-fork --session ID` 派发和人类可读／JSON 结果，不提供切点参数；增加解析／输出测试，覆盖不支持的切点、已确认／警告／unknown 结果和离线行为；验证 `./repo test --project cli`（AC-01、AC-03、AC-04、AC-06–AC-09、AC-11、AC-12）。
- [x] 3.3 增加长期维护的使用示例，以及共享目录／不创建 worktree 的说明，不承诺复制模型／预设；通过夹具驱动的 CLI 行为运行示例，确认合成逐字内容或原生事件序号不进入输出／证据（AC-01、AC-04、AC-11、AC-12）。

## 4. 集成与原生验收

- [x] 4.1 增加跨语言协议和全链路分叉场景，覆盖空闲／运行中来源、无完整轮次、子会话发现、独立订阅、重复、部分变更、重启和授权撤销；先验证 `./repo test --suite integration --group protocol`，再验证 `./repo test --suite integration`（AC-01–AC-12）。
- [x] 4.2 通过隔离的真实 DSH 配置，验证空闲／冷／运行中来源带后续排队任务时的最近完整轮次行为、源保持不变、子会话无复制的可执行收件箱、原生无完整轮次失败、子工作区警告和重载持久化；在 `.local-docs/acceptance/` 保存安全证据，发生冲突时保持能力不支持（AC-01–AC-03、AC-05、AC-07、AC-11、AC-12）。
- [x] 4.3 对变化的 core/cli/proto/access 项目运行支持的静态检查，对变化的每个 host/dsh/core/cli/access 包运行 `./repo test --project ID --coverage`；host/dsh 仅支持测试，MUST NOT 对其调用 check/build；每个适用包／crate 的实测覆盖率要求 90%，并验证 `./repo proto --check`（AC-01–AC-12）。
- [x] 4.4 根据新鲜证据更新所有验收结果行，评估是否偏离 DSH 所有权和创建基础，评估长期维护文档更新，并在宣称完成前报告任何未解决验收失败或原生／运行时限制（AC-01–AC-12）。


## Native parity regression work

- [x] 5.1 In the affected protocol, adapter, Host, Core and CLI paths already listed above, implement and verify shared READY lifecycle encoding, no dependency on enabling create, native inherited preset/default-model behavior, latest completed prefix and no automatic child run. Run affected ./repo unit/coverage checks, protocol checks and full integration as specified above, plus controlled native probes for source behavior; assess maintained-doc changes. Cover AC-01, AC-02, AC-07, AC-11. These are unexecuted tasks, not proof of native acceptance.


## Acceptance case matrix

Follow the [acceptance evidence contract](../../README.md#acceptance-evidence-contract). Case-family IDs below are planned: expand each semicolon-separated variant into a stable subcase ID, bind it to an exact automated test selector and (where required) a native procedure before acceptance, and attach every subresult to its parent AC row. Existing feature tasks own implementation; task 6.1 owns full matrix verification. Slashes name distinct required alternatives, not permission to test only one. Tests belong in the affected paths already identified above.

| AC | Planned case family | Mandatory variants | Required verification | Owner |
|---|---|---|---|---|
| AC-01 | `session-fork/AC-01.*` | idle-latest-complete-prefix; inherited-context; no-child-prompt | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-02 | `session-fork/AC-02.*` | parent-running; parent-queued; no-parent-interrupt; no-child-auto-run | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-03 | `session-fork/AC-03.*` | source-missing; source-has-no-complete-turn | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-04 | `session-fork/AC-04.*` | explicit-cut-argument; native-sequence-input; zero-native-call | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-05 | `session-fork/AC-05.*` | independent-history; reused-message-IDs; independent-watch/cursor/Attempt; parent-remains-live | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-06 | `session-fork/AC-06.*` | capability-absent; unauthorized; wrong-Host; expired-command | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-07 | `session-fork/AC-07.*` | confirmed-child-attach-warning; retained-child-and-parent; no-compensating-delete | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-08 | `session-fork/AC-08.*` | native-postchange-fault; lost-receipt; encoding/store-fault; unknown-no-inferred-ID | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-09 | `session-fork/AC-09.*` | concurrent-repeat; restart-repeat; changed-digest | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-10 | `session-fork/AC-10.*` | concurrent-distinct-command-IDs; distinct-child-IDs | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-11 | `session-fork/AC-11.*` | shared-directory; no-worktree; native-preset/default-model-not-forced-parent-copy | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-12 | `session-fork/AC-12.*` | offline-no-deferred-fork; synthetic-content/parameters/credentials-redaction; safe-durable-receipt | Automated fixtures/integration | 6.1 + existing AC-linked tasks |

- [x] 6.1 Implement the planned matrix subcases in the existing protocol/adapter/Host/Core/CLI test paths as applicable; record exact selectors, commands, assertions and native procedures. Run focused ./repo checks and full integration as required above, plus the mapped native procedures. Reconcile every subcase before setting a parent PASS; unresolved bounds or unavailable required native procedures remain BLOCKED. Assess capability gates and report partial delivery separately.

## 验收结果

| AC 编号 | 状态 | 证据 | 限制 |
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
| AC-12 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |

