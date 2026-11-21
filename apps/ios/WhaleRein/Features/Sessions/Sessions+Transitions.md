# Switcher transitions

| Event | From | To | Invariant |
| --- | --- | --- | --- |
| hostsLoadStarted | any | loading | Hosts and sessions load independently. |
| hostsLoaded(list) | loading | loaded(list) | A remembered Host survives; a vanished one falls back to the first. |
| hostsFailed(code) | loading | failed(code) | The selection is kept for retry. |
| hostSelected(id) | loaded | selected id | Unknown ids are ignored. |
| sessionsLoadStarted(hostId) | any | loading, cached cleared | Switching Host resets the cached indicator. |
| sessionsLoaded(list, cached) | loading | loaded, cached flag | Cached content carries the offline label. |
| sessionsFailed(code) | loading | failed(code) | Cached state is cleared. |
