# WhaleRein Conversation Glossary

Terms for WhaleRein's Harness-neutral conversation contract. DSH is the only
production Source currently; these definitions do not imply support for other
Harness integrations.

## Language

**Harness**:
The agent execution environment that owns conversation history and actions, such
as DSH or a future Pi integration.
_Avoid_: model provider

**Source**:
A configured Harness instance that owns a conversation and decides its history
and actions.
_Avoid_: provider (which may mean the model provider), backend

**Connector**:
The installed WhaleRein integration that exposes one Source instance remotely.
_Avoid_: machine, agent engine

**Host**:
The authenticated WhaleRein endpoint representing a Connector instance.
_Avoid_: physical computer

**Session**:
A Source-owned conversation with a stable identity within its Host.
_Avoid_: connection, channel, generation

**History View**:
The particular conversation history currently selected by the Source, including
its selected branch when the Source supports branching.
_Avoid_: complete native audit log, model context

**Message**:
An authoritative conversation item belonging to a History View.
_Avoid_: token, MQTT packet, generation attempt

**Attempt**:
One attempt to produce an assistant response; an interrupted or retried Attempt
need not produce a committed Message.
_Avoid_: reply, turn (whose meaning differs between Sources)

**Preview**:
Provisional output from an Attempt that can be corrected by an authoritative
Message and can disappear without changing committed conversation history.
_Avoid_: partially committed Message

**Run**:
A Source-level period of work that may include multiple Attempts, tools, retries
and queued follow-up work.
_Avoid_: Message, accepted command

**History Cursor**:
A Source-issued position in a particular History View from which synchronization
can continue.
_Avoid_: displayed event count, preview sequence

**History Cut**:
A fixed point and History View against which all pages of one synchronization
operation are read.
_Avoid_: whichever history is latest on each page

**Approval**:
A pending decision that the Source actually requires before allowing an action.
_Avoid_: generic UI confirmation, acknowledgement of an already executed action
