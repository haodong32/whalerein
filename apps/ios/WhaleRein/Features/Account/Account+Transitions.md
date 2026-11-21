# Account transitions

| Event | From | To | Invariant |
| --- | --- | --- | --- |
| statusLoadStarted | not loaded | loading | A loaded status is not replaced by a spinner. |
| statusLoaded(status) | any | loaded(status) | Default host follows the authoritative value. |
| statusFailed(code) | any | failed(code) | One status error. |
| defaultHostSelected(id) | any | id set, error cleared | Selection is optimistic until the service confirms. |
| defaultHostFailed(code) | any | error set | The previous selection stays visible. |
| sessionsLoadStarted | any | loading | Sessions are scoped to one instance. |
| sessionsLoaded(list) | any | loaded(list) | An empty list is a valid result. |
| sessionsFailed(code) | any | failed(code) | One sessions error. |
| revokeStarted(scope) | idle | revoking(scope) | One revocation at a time. |
| revokeSettled(result) | revoking | settled(result) | Listed sessions update from the reported state. |
| revokeFailed(scope, code) | revoking | failed(code) | The list is unchanged. |
