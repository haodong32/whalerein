# Protocol compatibility baseline changes

## Breaking change accepted

Rationale: Hard cut within application protocol v1 (OpenSpec complete-conversation-content D-05): replace the text-only notice/tool/live and flattened send-reply shapes with the single revised contract. No old-contract reader or compatibility bridge is shipped, so the prior baseline cannot describe a supported peer. Pairing, grants, source sessions, cursors and command recovery identities are retained; projection-versioned history is rebuilt by Core, and installed iOS builds on the prior contract are fenced.

- removed field ContentBlock.tool_call (tag 3) and reserved tag 3; the retired name stays reserved
- removed field ContentBlock.tool_result (tag 4) and reserved tag 4; the retired name stays reserved
- added `ContentBlock.tool_activity` (`ToolActivity`) at fresh tag 8 so an old peer decodes an unknown field instead of silently decoding the retired message type
- changed type_name of LiveFrame.delta: 'whalerein.v1.TextContent' -> 'whalerein.v1.LiveBlockDelta'
- changed type_name of LiveFrame.reset: 'whalerein.v1.TextContent' -> 'whalerein.v1.LiveBlockReset'
- tag 2 of MessageReply was reused from message_id to submitted_message_id
- changed json_name of MessageReply.message_id: 'messageId' -> 'submittedMessageId'
- removed field MessageReply.attempt_id (tag 4)
- removed field MessageReply.text (tag 5)
- changed type of NoticeContent.category: 'TYPE_STRING' -> 'TYPE_ENUM'
- changed type_name of NoticeContent.category: None -> 'whalerein.v1.NoticeCategory'
- tag 2 of NoticeContent was reused from text to state
- changed type of NoticeContent.text: 'TYPE_STRING' -> 'TYPE_ENUM'
- changed type_name of NoticeContent.text: None -> 'whalerein.v1.NoticeState'
- changed json_name of NoticeContent.text: 'text' -> 'state'
- tag 2 of ReadMessageReply was reused from message_id to submitted_message_id
- changed json_name of ReadMessageReply.message_id: 'messageId' -> 'submittedMessageId'
- removed message ToolCallContent
- removed message ToolResultContent

Compatible additions in the same reviewed change:

- added field `RequestTransportMetadata.revocation_generation` (tag 3); `grant_generation` and `revocation_generation` are both captured at submission and both must match for ciphertext release
## Breaking change accepted

Rationale: Coordinated relay epoch 3 hard cut: inline-or-reference transport replaces TransportFrame, fragment budgets and per-direction transport flags; application protocol version stays 1.

- removed field HostReady.transport_version (tag 4)
- removed field HostRequest.client_transport_version (tag 12)
- removed field HostRequest.relay_transport_version (tag 13)
- removed field TransportBudget.fragment_payload_bytes (tag 2)
- removed field TransportBudget.fragment_count (tag 3)
- removed field TransportBudget.fragment_idle_timeout_ms (tag 7)
- removed message TransportFrame
