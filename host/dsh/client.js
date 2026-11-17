window.__ModuleLoader__.load({
  id: 'whalerein-dsh',
  factory(require) {
    const React = require('react')
    const h = React.createElement
    // The host module table provides these components in DSH 0.2.2-rc.1.
    // Keep the browser smoke check when upgrading DSH: this is an internal API.
    const {
      Button, DisclosureRow, Menu, Input, StateDot, Tag, writeClipboard,
      IconCheckCircleOutlineRegular, IconWarningOutlineRegular,
      IconLinkOutlineRegular, IconChevronDownOutlineRegular,
      IconSettingsOutlineRegular, IconSlidersTwoOutlineRegular, IconCodeOutlineRegular,
    } = require('@deepseek-ai/dsh-client-ui-primitives')

    const NS = 'settings.whalerein'
    const ENTRY = 'whalerein-dsh'
    const FIELDS = [
      { key: 'server', label: 'server', subtitle: 'serverSubtitle', placeholder: 'serverPlaceholder', kind: 'text' },
      { key: 'host_id', label: 'hostId', subtitle: 'hostIdSubtitle', placeholder: 'hostIdPlaceholder', kind: 'text' },
      { key: 'host_token', label: 'hostToken', subtitle: 'hostTokenSubtitle', placeholder: 'tokenPlaceholder', kind: 'password' },
      { key: 'access_port', label: 'accessPort', subtitle: 'accessPortSubtitle', placeholder: 'accessPortPlaceholder', kind: 'text', advanced: true },
      { key: 'mqtt_port', label: 'transportPort', subtitle: 'transportPortSubtitle', placeholder: 'transportPortPlaceholder', kind: 'text', advanced: true },
      { key: 'ca_file', label: 'caFile', subtitle: 'caFileSubtitle', placeholder: 'caFilePlaceholder', kind: 'text', advanced: true },
      { key: 'log_level', label: 'logLevel', subtitle: 'logLevelSubtitle', kind: 'select', options: ['debug', 'info', 'warn', 'error', 'silent'], advanced: true },
    ]
    const SECRET_FIELDS = new Set(['host_token'])

    const en = {
      nav: 'WhaleRein',
      title: 'WhaleRein',
      intro: 'Securely control this DeepSeek Harness host from a paired device.',
      connected: 'Host connected to server',
      disconnected: 'Host not connected to server',
      disconnectedHint: 'The host reconnects automatically.',
      generate: 'Generate pairing link',
      generating: 'Generating…',
      pairingUri: 'Pairing link',
      pairingUriHint: 'Expires soon. One use.',
      copyUri: 'Copy pairing link',
      uriCopied: 'Pairing link copied.',
      copyFailed: 'Could not copy the link. Check browser clipboard permissions.',
      fingerprint: 'Host fingerprint',
      fingerprintHint: 'Compare this with the fingerprint shown by the client before confirming.',
      inviteKey: 'The host identity is not ready yet.',
      inviteServer: 'Save a server address first.',
      inviteUnauthorized: 'The server rejected this host.',
      inviteHost: 'This host is not registered on the server yet. Connect it first.',
      inviteFailed: 'The pairing link could not be generated.',
      devices: 'Paired devices',
      revoke: 'Revoke pairing',
      revokeConfirm: 'Revoke this device',
      revokeCancel: 'Cancel',
      revoking: 'Revoking',
      revokeKey: 'The host identity is not ready yet.',
      revokeUnauthorized: 'The server rejected this host.',
      revokeMissing: 'This device is no longer paired.',
      revokeFailed: 'Could not revoke this device. Try again.',
      deviceCount: '{count} devices',
      noDevices: 'No paired devices yet.',
      deviceConnected: 'Connected',
      devicePaired: 'Paired',
      pairedAt: 'Paired {time}',
      configuration: 'Connection settings',
      configurationHint: 'Empty ports use 443 and 8883, or 8080 and 1883 with plaintext. Some changes need a restart.',
      advanced: 'Advanced',
      server: 'Server',
      hostId: 'Host ID',
      hostToken: 'Host token',
      accessPort: 'Service port',
      transportPort: 'Transport port',
      caFile: 'CA bundle',
      logLevel: 'Log level',
      serverSubtitle: 'Access server hostname or IPv4 address.',
      hostIdSubtitle: 'Address used to identify this host. Leave blank to keep its current ID.',
      hostTokenSubtitle: 'Credential for connecting this host. Leave blank to keep the saved token.',
      accessPortSubtitle: 'Defaults to 443 with encryption, or 8080 with plaintext.',
      transportPortSubtitle: 'Defaults to 8883 with encryption, or 1883 with plaintext.',
      caFileSubtitle: 'Optional path to a CA bundle for verifying the server certificate.',
      logLevelSubtitle: 'Controls how much diagnostic information the plugin records.',
      serverPlaceholder: 'access.example',
      tokenPlaceholder: 'Leave blank to keep the current token',
      hostIdPlaceholder: 'host-1',
      accessPortPlaceholder: '443',
      transportPortPlaceholder: '8883',
      caFilePlaceholder: 'Optional CA bundle path',
      save: 'Save changes',
      saving: 'Saving…',
      testConnection: 'Test connection', testingConnection: 'Testing…',
      testSuccess: 'Connection test passed.',
      testFailure: '{stage} check failed: {reason}',
      testServiceFailure: 'Could not call the test service. Reload the plugin and try again.',
      testServiceUnavailable: 'The test service is not ready. Reload the plugin and try again.',
      testDraftFailure: 'Draft {stage} check failed: {reason}',
      testFailureOnline: '{stage} check failed: {reason}; the existing host connection is still online.',
      testStageConfiguration: 'Configuration', testStageAccess: 'Access service', testStageMqtt: 'Sign-in',
      test_invalid_config: 'Check the server, ports, host credentials and CA file.', test_timeout: 'Timed out',
      test_dns: 'Hostname could not be resolved', test_refused: 'Connection refused', test_tls: 'Certificate or TLS verification failed',
      test_not_ready: 'Access did not return readiness status 200', test_network: 'Network connection failed', test_busy: 'Another test is running. Try again shortly.',
      testCredentials: 'Sign-in rejected. Check the host token and server version.',
      testRateLimited: 'Please wait a moment before testing again.',
      tokenConfigured: 'Configured',
      tokenMissing: 'Not configured',
      saved: 'Settings saved.',
      saveFailed: 'The host rejected one or more values.',
      loading: 'Loading settings…',
      unavailable: 'WhaleRein is not loaded in this profile.',
      readOnly: 'This profile is read-only.',
      statusUnavailable: 'Connection status is temporarily unavailable. Check the settings below or try again.',
      configurationInvalidTitle: 'Saved configuration is invalid',
      configurationInvalidDetail: 'The host did not start: {field}{reason}. Fix the value below, save, then reload the plugin.',
      configurationInvalidGeneric: 'The saved configuration could not be resolved. Fix the connection settings below, save, then reload the plugin.',
      configurationInvalidField: 'the saved {field} value',
      configurationInvalidReason: ' ({reason})',
      externalChange: 'Settings changed in another window. Your unsaved draft is kept.',
      loadSaved: 'Load saved values',
      errorServerRequired: 'Enter a server address. Removing it requires a saved legacy MQTT URL.',
      errorServerInvalid: 'Use a host name or IPv4 address without a scheme or port.',
      errorPortInvalid: 'Use a whole number between 1 and 65535.',
      errorHostIdInvalid: 'Use 1-128 letters, digits, dots, colons, underscores or hyphens.',
      errorLogLevelInvalid: 'Choose one of the listed log levels.',
      errorSaveConflict: 'Settings changed in another window. Your draft was kept; review it and save again.',
      hostIdChange: 'Changing the host ID creates a new host identity. Paired devices must pair again and the host needs its provisioning token. Continue?',
      pairingUriExpires: 'Expires {time}. One use.',
      pairingUriExpired: 'This pairing link has expired. Generate a new one.',
      pairingUriUnknownExpiry: 'One use.',
      refresh: 'Refresh',
      diagnostics: 'Runtime diagnostics',
      diagnosticsHint: 'Current host state. MQTT counters accumulate during this host run; unavailable values are shown as —.',
      diagnosticHostId: 'Running host ID',
      diagnosticDsh: 'DSH availability',
      diagnosticIdentity: 'Host identity',
      diagnosticRevocations: 'Revocation sync',
      diagnosticReconnects: 'MQTT reconnects',
      diagnosticPublishErrors: 'MQTT publish failures',
      diagnosticOutbox: 'Pending encrypted envelopes',
      diagnosticUnknown: 'Commands with unknown outcome',
      diagnosticSnapshots: 'Sessions requiring resync',
      diagnosticReady: 'Ready',
      diagnosticUnavailable: 'Unavailable',
      diagnosticPending: 'Pending',
      diagnosticSynced: 'Synced',
      debugOnly: 'Debug',
      // Managed WhaleRein Cloud connection
      cloudIntro: 'Connect this computer to WhaleRein Cloud and approve it in your browser. Account sign-in stays in the browser.',
      cloudConnect: 'Connect to WhaleRein Cloud',
      cloudCancel: 'Cancel',
      cloudCancelling: 'Cancelling…',
      cloudTryAgain: 'Try again',
      cloudReconnect: 'Reconnect',
      cloudDisconnect: 'Disconnect',
      cloudDisconnecting: 'Disconnecting…',
      cloudRemove: 'Remove this computer',
      cloudRemoving: 'Removing…',
      cloudOpenBrowser: 'Open browser',
      cloudBrowserBlocked: 'Your browser blocked the new tab. Use Open browser to finish connecting.',
      cloudApprovalLink: 'Approval link',
      cloudActionFailed: 'The connection action could not be completed. Try again.',
      cloudStatusUnavailable: 'Connection status is temporarily unavailable. Try again in a moment.',
      cloudPending: 'A connection operation is already in progress.',
      cloudDismiss: 'Dismiss',
      state_idle: 'Not connected',
      state_requesting: 'Preparing connection…',
      state_awaiting_approval: 'Finish connecting in your browser',
      state_enrolling: 'Connecting…',
      state_recovering: 'Recovering connection…',
      state_connecting: 'Connecting…',
      state_connected: 'Connected to WhaleRein Cloud',
      state_reconnecting: 'Reconnecting…',
      state_disconnected: 'Disconnected',
      state_denied: 'Connection request denied',
      state_expired: 'Connection request expired',
      state_action_required: 'Action required',
      state_removing: 'Removing this computer…',
      state_removed: 'This computer was removed',
      stateHint_idle: 'Connect this computer to WhaleRein Cloud. Browser approval starts only after you choose Connect.',
      stateHint_requesting: 'Preparing a secure connection request. Do not close this page.',
      stateHint_awaiting_approval: 'Approve this computer in the browser tab we opened. Keep this page open.',
      stateHint_enrolling: 'Approval received. Establishing this computer’s connection.',
      stateHint_recovering: 'Recovering a pending connection. This can take a moment.',
      stateHint_connecting: 'Establishing the connection. Connected appears only after the service confirms readiness.',
      stateHint_connected: 'This computer is connected and ready for remote control.',
      stateHint_reconnecting: 'The connection was interrupted. Automatic reconnects continue within the current authorization.',
      stateHint_disconnected: 'This computer is disconnected locally. Its cloud registration is retained; Connect resumes the same identity.',
      stateHint_denied: 'The request was denied in the browser. Try again to create a new approval request.',
      stateHint_expired: 'The request expired before it was approved. Try again to create a new approval request.',
      stateHint_action_required: 'One action is needed before this computer can connect.',
      stateHint_removing: 'Waiting for the service to enforce removal. This can take a moment.',
      stateHint_removed: 'Removal was enforced. Adding this computer again requires fresh browser approval.',
      cloudDetails: 'Connection details',
      cloudDetailsHint: 'Non-secret identity and status only. Credentials, keys, routes and host identifiers are never shown.',
      cloudDetailsEnvironment: 'Environment',
      cloudDetailsProfile: 'Profile',
      cloudDetailsHostName: 'This computer',
      cloudDetailsState: 'Status',
      cloudDetailsRegion: 'Region',
      cloudDetailsOrigin: 'Service origin',
      cloudDetailsRevision: 'Registration revision',
      cloudDetailsLastError: 'Last error code',
      cloudDetailsRefresh: 'Refresh details',
      cloudDetailsUnavailable: 'Details are unavailable right now.',
      cloudAdvanced: 'Advanced',
      cloudCustomService: 'Custom service',
      cloudCustomServiceHint: 'Use this only for a reviewed self-hosted WhaleRein service. Adding it does not activate it.',
      cloudCustomLabel: 'Service label',
      cloudCustomOrigin: 'API origin',
      cloudCustomOriginPlaceholder: 'https://service.example',
      cloudAddCustomService: 'Add custom service',
      cloudCustomServiceAdded: 'Custom service added. Select it from the profile list to use it.',
      cloudDetailsDesired: 'Desired environment',
      cloudDetailsSource: 'Configuration source',
      cloudSourceBuiltin: 'Official catalog',
      cloudSourceFile: 'Per-device file',
      cloudDeveloperTools: 'Developer tools',
      cloudDeveloperToolsHint: 'Test environments are hidden until you enable them here.',
      cloudLocal: 'Local development',
      cloudUseLocal: 'Use local development',
      cloudEnableStaging: 'Enable staging',
      cloudEnablingStaging: 'Enabling…',
      cloudStagingNote: 'Enabling staging does not switch this computer. A staging profile must be selected explicitly.',
      cloudStagingProfile: 'WhaleRein Cloud Staging',
      cloudUseStaging: 'Use staging',
      cloudSwitchProduction: 'Switch to production',
      cloudSwitching: 'Switching…',
      cloudStagingActive: 'Staging',
      cloudProduction: 'Production',
      cloudCustom: 'Custom',
      cloudProfiles: 'Local profiles',
      cloudProfilesHint: 'Only an inactive local profile can be deleted. Deleting it does not remove the cloud registration.',
      cloudNoProfiles: 'No local profiles are available.',
      cloudDeleteProfile: 'Delete local profile',
      cloudDeleteActive: 'Switch away from this profile before deleting it.',
      cloudDeleteHint: 'Deleting local files does not revoke this computer in the cloud. Use Remove this computer for that.',
      cloudRemovedLocal: 'Local profile deleted. Its remote registration remains.',
      cloudPairingManaged: 'Device pairing is managed by WhaleRein Cloud for this connection.',
      cloudDisconnectHint: 'Disconnect only pauses this computer locally. It keeps its registration and can reconnect without new approval.',
      cloudRemoveHint: 'Remove this computer requires fresh browser authorization. The status stays Removing until the service enforces it.',
      cloudLocalOnly: 'Local action',
      cloudBrowserAuthorized: 'Browser-authorized action',
      cloudUnsupported: 'This action is not available for the selected connection.',
      cloudRetryHint: 'This creates a new browser approval request.',
      cloudConfirmationRequired: 'Confirm removal in your browser. Keep this page open.',
    }
    const zh = {
      nav: 'WhaleRein',
      title: 'WhaleRein',
      intro: '通过已配对的设备，安全地远程控制当前 DeepSeek Harness 主机。',
      connected: '当前主机已连接服务器',
      disconnected: '当前主机未连接服务器',
      disconnectedHint: '主机会自动重新连接。',
      generate: '生成配对链接',
      generating: '正在生成…',
      pairingUri: '配对链接',
      pairingUriHint: '很快过期，只能使用一次。',
      copyUri: '复制配对链接',
      uriCopied: '已复制配对链接。',
      copyFailed: '无法复制链接，请检查浏览器剪贴板权限。',
      fingerprint: '主机指纹',
      fingerprintHint: '客户端确认配对时，核对此指纹。',
      inviteKey: '主机身份还没有准备好。',
      inviteServer: '请先保存服务器地址。',
      inviteUnauthorized: '服务器拒绝了这台主机。',
      inviteHost: '服务器上还没有这台主机，请先确认主机已连接。',
      inviteFailed: '暂时无法生成配对链接。',
      devices: '已配对设备',
      revoke: '撤销配对',
      revokeConfirm: '确认撤销',
      revokeCancel: '取消',
      revoking: '正在撤销',
      revokeKey: '主机身份还没有准备好。',
      revokeUnauthorized: '服务器拒绝了这台主机。',
      revokeMissing: '这台设备已经不在配对中。',
      revokeFailed: '暂时无法撤销这台设备，请重试。',
      deviceCount: '{count} 台设备',
      noDevices: '暂无已配对设备。',
      deviceConnected: '已连接',
      devicePaired: '已配对',
      pairedAt: '配对于 {time}',
      configuration: '连接配置',
      configurationHint: '端口留空时，加密用 443 和 8883，明文用 8080 和 1883。部分变更需要重启。',
      advanced: '高级配置',
      server: '服务器',
      hostId: '主机 ID',
      hostToken: '主机 Token',
      accessPort: '服务端口',
      transportPort: '传输端口',
      caFile: 'CA 证书路径',
      logLevel: '日志级别',
      serverSubtitle: 'Access 服务器的主机名或 IPv4 地址。',
      hostIdSubtitle: '用于定位当前主机的地址，留空可保留现有 ID。',
      hostTokenSubtitle: '连接主机所需的凭据，留空可保留已保存的 Token。',
      accessPortSubtitle: '留空时，加密连接使用 443，明文连接使用 8080。',
      transportPortSubtitle: '留空时，加密连接使用 8883，明文连接使用 1883。',
      caFileSubtitle: '用于验证服务器证书的 CA 证书文件路径，可留空。',
      logLevelSubtitle: '控制插件记录诊断信息的详细程度。',
      serverPlaceholder: 'access.example',
      tokenPlaceholder: '留空表示保持当前值',
      hostIdPlaceholder: 'host-1',
      accessPortPlaceholder: '443',
      transportPortPlaceholder: '8883',
      caFilePlaceholder: '可选的 CA 证书路径',
      save: '保存修改',
      saving: '保存中…',
      testConnection: '测试连接', testingConnection: '测试中…',
      testSuccess: '连接测试通过。',
      testFailure: '{stage}检查失败：{reason}',
      testServiceFailure: '测试接口调用失败，请重新加载插件后重试。',
      testServiceUnavailable: '测试接口尚未就绪，请重新加载插件后重试。',
      testDraftFailure: '草稿检查失败（{stage}）：{reason}',
      testFailureOnline: '{stage}检查失败：{reason}；现有主机连接仍在线。',
      testStageConfiguration: '配置', testStageAccess: 'Access 服务', testStageMqtt: '登录验证',
      test_invalid_config: '请检查服务器、端口、主机凭据和 CA 文件。', test_timeout: '连接超时',
      test_dns: '无法解析服务器地址', test_refused: '连接被拒绝', test_tls: '证书或 TLS 验证失败',
      test_not_ready: 'Access 未返回就绪状态 200', test_network: '网络连接失败', test_busy: '已有测试正在运行，请稍后重试。',
      testCredentials: '登录被拒绝，请检查主机 Token 和服务端版本。',
      testRateLimited: '测试过于频繁，请稍后重试。',
      tokenConfigured: '已配置',
      tokenMissing: '未配置',
      saved: '设置已保存。',
      saveFailed: '主机拒绝了部分值。',
      loading: '正在加载设置…',
      unavailable: '当前 profile 未加载 WhaleRein。',
      readOnly: '当前 profile 只读。',
      statusUnavailable: '暂时无法获取连接状态，请检查下方配置或重试。',
      configurationInvalidTitle: '已保存的配置无效',
      configurationInvalidDetail: '主机未能启动：{field}{reason}。请修正下方对应字段，保存后重新加载插件。',
      configurationInvalidGeneric: '无法解析已保存的配置。请修正下方连接配置，保存后重新加载插件。',
      configurationInvalidField: '已保存的 {field}',
      configurationInvalidReason: '（{reason}）',
      externalChange: '设置已在另一个窗口修改，你未保存的草稿已保留。',
      loadSaved: '载入已保存的值',
      errorServerRequired: '请输入服务器地址。清空服务器前，profile 中必须已保存旧版 MQTT 地址。',
      errorServerInvalid: '请填写不含协议和端口的主机名或 IPv4 地址。',
      errorPortInvalid: '请填写 1 到 65535 之间的整数。',
      errorHostIdInvalid: '请使用 1-128 个字母、数字、点、冒号、下划线或连字符。',
      errorLogLevelInvalid: '请选择列表中的日志级别。',
      errorSaveConflict: '设置已在另一个窗口修改，你的草稿已保留，请检查后重新保存。',
      hostIdChange: '修改主机 ID 会创建新的主机身份，已配对设备需要重新配对，并且主机需要重新配置 Token。是否继续？',
      pairingUriExpires: '{time} 过期，只能使用一次。',
      pairingUriExpired: '该配对链接已过期，请重新生成。',
      pairingUriUnknownExpiry: '只能使用一次。',
      refresh: '刷新',
      diagnostics: '运行诊断',
      diagnosticsHint: '当前主机的运行状态。MQTT 计数在本次运行期间累计；无法获取的值显示为 —。',
      diagnosticHostId: '运行中的主机 ID',
      diagnosticDsh: 'DSH 可用性',
      diagnosticIdentity: '主机身份',
      diagnosticRevocations: '撤销记录同步',
      diagnosticReconnects: 'MQTT 重连次数',
      diagnosticPublishErrors: 'MQTT 发布失败数',
      diagnosticOutbox: '待发送密文数',
      diagnosticUnknown: '结果未知的命令数',
      diagnosticSnapshots: '需要重新同步的会话数',
      diagnosticReady: '就绪',
      diagnosticUnavailable: '不可用',
      diagnosticPending: '等待同步',
      diagnosticSynced: '已同步',
      debugOnly: '调试',
      // Managed WhaleRein Cloud connection
      cloudIntro: '将这台电脑连接到 WhaleRein Cloud，并在浏览器中授权。账号登录始终留在浏览器中。',
      cloudConnect: '连接到 WhaleRein Cloud',
      cloudCancel: '取消',
      cloudCancelling: '正在取消…',
      cloudTryAgain: '重试',
      cloudReconnect: '重新连接',
      cloudDisconnect: '断开连接',
      cloudDisconnecting: '正在断开…',
      cloudRemove: '移除这台电脑',
      cloudRemoving: '正在移除…',
      cloudOpenBrowser: '打开浏览器',
      cloudBrowserBlocked: '浏览器阻止了新标签页。请使用“打开浏览器”完成连接。',
      cloudApprovalLink: '授权链接',
      cloudActionFailed: '无法完成连接操作，请重试。',
      cloudStatusUnavailable: '暂时无法获取连接状态，请稍后重试。',
      cloudPending: '已有连接操作正在进行。',
      cloudDismiss: '关闭',
      state_idle: '未连接',
      state_requesting: '正在准备连接…',
      state_awaiting_approval: '请在浏览器中完成连接',
      state_enrolling: '正在连接…',
      state_recovering: '正在恢复连接…',
      state_connecting: '正在连接…',
      state_connected: '已连接到 WhaleRein Cloud',
      state_reconnecting: '正在重新连接…',
      state_disconnected: '已断开连接',
      state_denied: '连接请求被拒绝',
      state_expired: '连接请求已过期',
      state_action_required: '需要处理',
      state_removing: '正在移除这台电脑…',
      state_removed: '这台电脑已被移除',
      stateHint_idle: '连接这台电脑到 WhaleRein Cloud。只有选择“连接”后才会开始浏览器授权。',
      stateHint_requesting: '正在准备安全的连接请求，请不要关闭此页面。',
      stateHint_awaiting_approval: '请在我们打开的浏览器标签页中授权这台电脑，并保持此页面打开。',
      stateHint_enrolling: '已收到授权，正在建立这台电脑的连接。',
      stateHint_recovering: '正在恢复待处理的连接，请稍候。',
      stateHint_connecting: '正在建立连接。只有服务确认就绪后才会显示已连接。',
      stateHint_connected: '这台电脑已连接，可以进行远程控制。',
      stateHint_reconnecting: '连接中断，将在当前授权范围内自动重连。',
      stateHint_disconnected: '这台电脑已在本地断开，云端注册仍保留；点击连接会恢复同一身份。',
      stateHint_denied: '请求已在浏览器中被拒绝。重试会创建新的授权请求。',
      stateHint_expired: '请求在授权前已过期。重试会创建新的授权请求。',
      stateHint_action_required: '需要完成一个操作才能建立连接。',
      stateHint_removing: '正在等待服务执行移除，请稍候。',
      stateHint_removed: '移除已生效。重新添加这台电脑需要新的浏览器授权。',
      cloudDetails: '连接详情',
      cloudDetailsHint: '仅显示非敏感的身份与状态。凭据、密钥、路由和主机标识永远不会显示。',
      cloudDetailsEnvironment: '环境',
      cloudDetailsProfile: '配置',
      cloudDetailsHostName: '这台电脑',
      cloudDetailsState: '状态',
      cloudDetailsRegion: '区域',
      cloudDetailsOrigin: '服务来源',
      cloudDetailsRevision: '注册版本',
      cloudDetailsLastError: '最近错误代码',
      cloudDetailsRefresh: '刷新详情',
      cloudDetailsUnavailable: '暂时无法获取详情。',
      cloudAdvanced: '高级配置',
      cloudCustomService: '自定义服务',
      cloudCustomServiceHint: '仅用于经过审核的自托管 WhaleRein 服务。添加后不会自动启用。',
      cloudCustomLabel: '服务名称',
      cloudCustomOrigin: 'API 来源',
      cloudCustomOriginPlaceholder: 'https://service.example',
      cloudAddCustomService: '添加自定义服务',
      cloudCustomServiceAdded: '已添加自定义服务。请在配置列表中选择后才会使用。',
      cloudDetailsDesired: '目标环境',
      cloudDetailsSource: '配置来源',
      cloudSourceBuiltin: '官方目录',
      cloudSourceFile: '本机文件',
      cloudDeveloperTools: '开发者工具',
      cloudDeveloperToolsHint: '测试环境默认隐藏，需要在这里显式启用。',
      cloudLocal: '本地开发',
      cloudUseLocal: '使用本地开发',
      cloudEnableStaging: '启用 Staging',
      cloudEnablingStaging: '正在启用…',
      cloudStagingNote: '启用 Staging 不会切换这台电脑，必须显式选择 Staging 配置。',
      cloudStagingProfile: 'WhaleRein Cloud Staging',
      cloudUseStaging: '使用 Staging',
      cloudSwitchProduction: '切换回生产环境',
      cloudSwitching: '正在切换…',
      cloudStagingActive: 'Staging',
      cloudProduction: '生产环境',
      cloudCustom: '自定义',
      cloudProfiles: '本地配置',
      cloudProfilesHint: '只能删除未启用的本地配置。删除本地配置不会移除云端注册。',
      cloudNoProfiles: '暂无本地配置。',
      cloudDeleteProfile: '删除本地配置',
      cloudDeleteActive: '请先切换到其他配置再删除。',
      cloudDeleteHint: '删除本地文件不会在云端撤销这台电脑。如需撤销，请使用“移除这台电脑”。',
      cloudRemovedLocal: '本地配置已删除，云端注册仍然保留。',
      cloudPairingManaged: '此连接的设备配对由 WhaleRein Cloud 管理。',
      cloudDisconnectHint: '断开连接只会在本地暂停这台电脑，云端注册仍保留，无需重新授权即可连接。',
      cloudRemoveHint: '移除这台电脑需要新的浏览器授权。在服务执行完成前，状态会一直显示“正在移除”。',
      cloudLocalOnly: '本地操作',
      cloudBrowserAuthorized: '浏览器授权操作',
      cloudUnsupported: '所选连接不支持此操作。',
      cloudRetryHint: '这会创建新的浏览器授权请求。',
      cloudConfirmationRequired: '请在浏览器中确认移除，并保持此页面打开。',
    }

    const styles = {
      page: { display: 'flex', flexDirection: 'column', gap: '24px', width: '100%', color: 'var(--dsw-alias-label-primary)', fontSize: '14px', lineHeight: '22px' },
      header: { display: 'flex', flexDirection: 'column', gap: '6px' },
      title: { margin: 0, fontSize: '20px', fontWeight: 500, lineHeight: '28px' },
      intro: { margin: 0, color: 'var(--dsw-alias-label-secondary)', fontSize: '13px', lineHeight: '20px' },
      section: { display: 'flex', flexDirection: 'column', gap: '12px' },
      sectionHeader: { display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: '12px' },
      sectionTitle: { margin: 0, fontSize: '14px', fontWeight: 500, lineHeight: '22px' },
      hint: { margin: 0, color: 'var(--dsw-alias-label-tertiary)', fontSize: '12px', lineHeight: '18px' },
      statusCard: { display: 'flex', flexDirection: 'column', gap: '16px', padding: '20px', border: '0.5px solid var(--dsw-alias-settings-card-stroke)', borderRadius: 'var(--dsw-radius-xl)', background: 'var(--dsw-alias-settings-card-fill)' },
      connectionHeader: { display: 'flex', alignItems: 'center', justifyContent: 'space-between', flexWrap: 'wrap', gap: '16px' },
      connectionIdentity: { display: 'flex', alignItems: 'center', justifyContent: 'space-between', flexWrap: 'wrap', gap: '12px 24px', paddingTop: '16px', borderTop: '0.5px solid var(--dsw-alias-border-l2)' },
      connectionCopy: { display: 'flex', flexDirection: 'column', gap: '4px', flex: '1 1 180px', minWidth: 0 },
      pairingDetails: { display: 'flex', flexDirection: 'column', gap: '12px', paddingTop: '16px', borderTop: '0.5px solid var(--dsw-alias-border-l2)' },
      statusHeader: { display: 'flex', alignItems: 'center', flex: '1 1 180px', minWidth: 0, gap: '10px' },
      statusIcon: { display: 'inline-flex', flexShrink: 0 },
      statusText: { display: 'flex', flexDirection: 'column', gap: '2px', minWidth: 0 },
      statusTitle: { fontSize: '14px', fontWeight: 500, lineHeight: '20px' },
      statusHint: { color: 'var(--dsw-alias-label-tertiary)', fontSize: '12px', lineHeight: '18px' },
      codeLabel: { margin: 0, color: 'var(--dsw-alias-label-primary)', fontSize: '13px', fontWeight: 500, lineHeight: '20px' },
      code: { display: 'block', margin: 0, padding: '10px 12px', borderRadius: 'var(--dsw-radius-md)', background: 'var(--dsw-alias-bg-layer-1)', color: 'var(--dsw-alias-label-primary)', fontFamily: 'var(--ds-font-family-code, ui-monospace, monospace)', fontSize: '13px', lineHeight: '20px', overflowWrap: 'anywhere', userSelect: 'all' },
      fingerprint: { margin: 0, color: 'var(--dsw-alias-label-secondary)', fontFamily: 'var(--dsw-font-mono, ui-monospace, SFMono-Regular, Menlo, monospace)', fontSize: '14px', lineHeight: '22px', letterSpacing: '0.02em', overflowWrap: 'anywhere', userSelect: 'all' },
      error: { color: 'var(--dsw-alias-state-error-primary)', fontSize: '12px', lineHeight: '18px', margin: 0 },
      success: { color: 'var(--dsw-alias-state-success-primary)', fontSize: '12px', lineHeight: '18px', margin: 0 },
      deviceList: { margin: 0, padding: 0, listStyle: 'none', borderTop: '1px solid var(--dsw-alias-border-l2)' },
      deviceRow: { display: 'flex', alignItems: 'center', flexWrap: 'wrap', gap: '12px', minWidth: 0, padding: '12px 0', borderBottom: '1px solid var(--dsw-alias-border-l2)' },
      deviceIcon: { display: 'flex', alignItems: 'center', justifyContent: 'center', flex: 'none', width: '32px', height: '32px', borderRadius: 'var(--dsw-radius-md)', color: 'var(--dsw-alias-label-secondary)', background: 'var(--dsw-alias-bg-layer-2)' },
      deviceMain: { flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column', gap: '2px' },
      deviceHeading: { display: 'flex', alignItems: 'center', gap: '8px', minWidth: 0 },
      deviceName: { overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', minWidth: 0, fontSize: '13px', fontWeight: 500, lineHeight: '20px' },
      deviceMeta: { color: 'var(--dsw-alias-label-tertiary)', fontSize: '12px', lineHeight: '18px', overflowWrap: 'anywhere' },
      deviceId: { fontFamily: 'var(--dsw-font-mono, ui-monospace, SFMono-Regular, Menlo, monospace)' },
      deviceActions: { display: 'flex', alignItems: 'center', flexWrap: 'wrap', gap: '8px' },
      revokeAction: { color: 'var(--dsw-alias-state-error-primary)', whiteSpace: 'nowrap', flexShrink: 0 },
      // Button has no danger variant. Override its primary palette locally so
      // the official control still owns hover, focus and disabled behavior.
      revokeConfirm: { '--dsw-alias-button-primary-fill': 'var(--dsw-alias-state-error-primary)', '--dsw-alias-button-primary-hover': 'color-mix(in srgb, var(--dsw-alias-state-error-primary) 88%, var(--dsw-alias-label-primary))', whiteSpace: 'nowrap', flexShrink: 0 },
      fieldRow: { display: 'grid', alignItems: 'center', gap: '24px', padding: '16px 0', borderBottom: '0.5px solid var(--dsw-alias-border-l2)' },
      fieldCopy: { minWidth: 0, display: 'flex', flexDirection: 'column', gap: '4px' },
      fieldTitle: { display: 'flex', alignItems: 'center', flexWrap: 'wrap', gap: '8px', fontSize: '14px', fontWeight: 400, lineHeight: '20px' },
      fieldSubtitle: { margin: 0, color: 'var(--dsw-alias-label-secondary)', fontSize: '12px', lineHeight: '18px' },
      fieldControl: { minWidth: 0, display: 'flex', justifyContent: 'flex-end' },
      formActions: { display: 'flex', alignItems: 'center', flexWrap: 'wrap', gap: '8px', paddingTop: '16px' },
      details: { borderTop: '1px solid var(--dsw-alias-border-l2)', paddingTop: '16px' },
      form: { display: 'flex', flexDirection: 'column', gap: '16px', paddingTop: '12px' },
      diagnosticRow: { display: 'grid', gap: '16px', padding: '8px 0', fontSize: '13px' },
      managedActions: { display: 'flex', alignItems: 'center', flexWrap: 'wrap', gap: '8px', minWidth: 0 },
      managedStateRegion: { display: 'flex', flexDirection: 'column', gap: '4px', minWidth: 0, outlineOffset: '2px' },
      stagingLabel: { display: 'inline-flex', alignItems: 'center', gap: '6px', flexWrap: 'wrap' },
      approvalLink: { display: 'inline-flex', alignItems: 'center', minHeight: '32px', maxWidth: '100%', padding: '0 12px', borderRadius: 'var(--dsw-radius-md)', background: 'var(--dsw-alias-bg-layer-1)', color: 'var(--dsw-alias-label-primary)', textDecoration: 'underline', overflowWrap: 'anywhere' },
      profileRow: { display: 'flex', alignItems: 'center', justifyContent: 'space-between', flexWrap: 'wrap', gap: '8px 12px', padding: '12px 0', borderBottom: '0.5px solid var(--dsw-alias-border-l2)' },
      profileCopy: { display: 'flex', flexDirection: 'column', gap: '2px', minWidth: 0, flex: '1 1 180px' },
      profileName: { fontSize: '13px', fontWeight: 500, lineHeight: '20px', overflowWrap: 'anywhere' },
      detailValue: { margin: 0, overflowWrap: 'anywhere' },
      dangerAction: { color: 'var(--dsw-alias-state-error-primary)' },
    }

    const css = `
      /* Match the settings Models inputs and General Language selector. */
      .whalerein-settings .whalerein-input { box-sizing: border-box; width: 100%; min-width: 0; height: 32px; padding: 0 10px; }
      .whalerein-settings .whalerein-input input { font: inherit; font-size: 14px; line-height: 22px; }
      .whalerein-settings .whalerein-input:has(input:disabled) { opacity: 0.6; }
      .whalerein-settings .whalerein-select { gap: 12px; background: var(--dsw-alias-bg-module-platform); white-space: nowrap; }
      @media (hover: hover) {
        .whalerein-settings .whalerein-select:hover:not(:disabled) { background: var(--dsw-alias-interactive-bg-hover); }
      }
      .whalerein-settings .whalerein-field-row { grid-template-columns: minmax(0, 1fr) minmax(180px, 240px); }
      .whalerein-settings .whalerein-diagnostic-row { grid-template-columns: minmax(140px, 0.7fr) minmax(0, 1.3fr); }
      @media (max-width: 560px) {
        .whalerein-settings .whalerein-field-row { grid-template-columns: minmax(0, 1fr); gap: 12px; }
        .whalerein-settings .whalerein-field-control { justify-content: flex-start; }
        .whalerein-settings .whalerein-diagnostic-row { grid-template-columns: minmax(0, 1fr); gap: 6px; }
      }
      /* 320 CSS px and 200% zoom: actions wrap and text never forces width. */
      .whalerein-settings .whalerein-managed-actions { flex-wrap: wrap; }
      .whalerein-settings .whalerein-approval-link { max-width: 100%; overflow-wrap: anywhere; }
      .whalerein-settings .whalerein-profile-row { flex-wrap: wrap; }
      @media (max-width: 400px) {
        .whalerein-settings { gap: 16px; }
        .whalerein-settings .whalerein-managed-actions { align-items: stretch; }
        .whalerein-settings .whalerein-managed-actions > * { max-width: 100%; }
      }
    `

    function initialDraft(value) {
      const draft = {}
      for (const field of FIELDS) {
        if (SECRET_FIELDS.has(field.key)) {
          draft[field.key] = ''
        } else {
          draft[field.key] = value?.[field.key] ?? (field.key === 'log_level' ? 'info' : '')
        }
      }
      return draft
    }

    function inviteErrorKey(code) {
      if (code === 'whalerein/host-key-missing') return 'inviteKey'
      if (code === 'whalerein/server-missing') return 'inviteServer'
      if (code === 'whalerein/unauthorized') return 'inviteUnauthorized'
      if (code === 'whalerein/host-not-found') return 'inviteHost'
      return 'inviteFailed'
    }

    function revokeErrorKey(code) {
      if (code === 'whalerein/host-key-missing') return 'revokeKey'
      if (code === 'whalerein/unauthorized') return 'revokeUnauthorized'
      if (code === 'whalerein/not-found') return 'revokeMissing'
      return 'revokeFailed'
    }

    function Field({ field, draft, t, onChange, secretSet, disabled, id, menuOpen, setMenuOpen, error }) {
      const label = t(field.label)
      const subtitleId = `${id}-subtitle`
      const errorId = `${id}-error`
      const describedBy = error ? `${subtitleId} ${errorId}` : subtitleId
      let control
      if (field.kind === 'select') {
        control = h(Menu, {
          open: menuOpen && !disabled,
          anchor: h(Button, {
            variant: 'ghost', size: 'md', className: 'whalerein-select', disabled,
            'aria-labelledby': `${id}-label ${id}-value`,
            'aria-describedby': describedBy,
            'aria-haspopup': 'menu', 'aria-expanded': menuOpen && !disabled,
            onClick: () => setMenuOpen((open) => !open),
          }, h('span', { id: `${id}-value` }, draft[field.key]), h(IconChevronDownOutlineRegular)),
          items: field.options.map((value) => ({ id: value, label: value })),
          selectedId: draft[field.key], align: 'end', autoFocus: true, portal: true,
          onClose: () => setMenuOpen(false),
          onSelect: (value) => { onChange(field.key, value); setMenuOpen(false) },
        })
      } else {
        control = h(Input, {
          id, disabled, type: field.kind === 'password' ? 'password' : 'text',
          className: 'whalerein-input', 'aria-label': label, 'aria-describedby': describedBy,
          value: draft[field.key] ?? '',
          placeholder: field.placeholder ? t(field.placeholder) : undefined,
          autoComplete: field.kind === 'password' ? 'new-password' : 'off',
          onChange: (event) => onChange(field.key, event.target.value),
        })
      }
      return h('div', { style: styles.fieldRow, className: 'whalerein-field-row' },
        h('div', { style: styles.fieldCopy },
          h('div', { style: styles.fieldTitle },
            field.kind === 'select'
              ? h('span', { id: `${id}-label` }, label)
              : h('label', { htmlFor: id }, label),
            field.kind === 'password' ? h(Tag, { tone: secretSet ? 'neutral' : 'quiet' }, t(secretSet ? 'tokenConfigured' : 'tokenMissing')) : null),
          h('p', { id: subtitleId, style: styles.fieldSubtitle }, t(field.subtitle)),
          error ? h('p', { id: errorId, style: styles.error, role: 'alert' }, error) : null),
        h('div', { style: styles.fieldControl, className: 'whalerein-field-control' }, control))
    }

    function formatTime(value, locale) {
      const date = new Date(value)
      if (!Number.isFinite(date.getTime())) return ''
      try {
        return new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short' }).format(date)
      } catch {
        return date.toLocaleString()
      }
    }

    function osLabel(value) {
      const labels = {
        macos: 'macOS',
        ios: 'iOS',
        linux: 'Linux',
        windows: 'Windows',
        android: 'Android',
        freebsd: 'FreeBSD',
        openbsd: 'OpenBSD',
        netbsd: 'NetBSD',
        dragonfly: 'DragonFly',
        solaris: 'Solaris',
      }
      return labels[String(value ?? '').trim().toLowerCase()] ?? ''
    }

    function DeviceRow({ device, t, locale, confirming, revoking, busy, autoFocus, id, error, onAsk, onCancel, onConfirm }) {
      const time = formatTime(device.pairedAt, locale)
      const name = String(device.displayName ?? '').trim()
      // Two devices often share the default computer name. Always expose a
      // short stable id in the heading so a revoke cannot target a same-named
      // online device by mistake; the full id stays in the metadata line.
      const tail = device.deviceId.slice(device.deviceId.lastIndexOf('-') + 1)
      const shortId = tail.length >= 6 ? tail : device.deviceId.slice(-8)
      const title = name ? `${name} · ${shortId}` : device.deviceId
      const os = osLabel(device.os)
      const identity = name ? h('span', { style: styles.deviceId }, device.deviceId) : null
      const status = h(Tag, { tone: device.connected ? 'success' : 'quiet' },
        h(StateDot, { state: device.connected ? 'done' : 'idle', size: 10 }),
        device.connected ? t('deviceConnected') : t('devicePaired'))
      const revokeControls = confirming
        ? h('div', {
          style: styles.deviceActions,
          onKeyDown: (event) => {
            if (event?.key === 'Escape' && !revoking) {
              event.preventDefault?.()
              onCancel()
            }
          },
        },
          h(Button, { variant: 'ghost', size: 'sm', disabled: revoking, autoFocus: true, onClick: onCancel }, t('revokeCancel')),
          h(Button, { variant: 'primary', size: 'sm', style: styles.revokeConfirm, disabled: revoking, onClick: onConfirm }, revoking ? t('revoking') : t('revokeConfirm')))
        : h('span', { id, style: { display: 'contents' } },
          h(Button, { variant: 'ghost', size: 'sm', style: styles.revokeAction, disabled: busy, autoFocus, onClick: onAsk }, t('revoke')))
      return h('li', { style: styles.deviceRow, className: 'whalerein-device-row' },
        h('div', { style: styles.deviceIcon }, h(IconLinkOutlineRegular, { size: 17 })),
        h('div', { style: styles.deviceMain },
          h('div', { style: styles.deviceHeading },
            h('div', { style: styles.deviceName, title }, title),
            status),
          os || identity ? h('div', { style: styles.deviceMeta }, os && identity ? [os, ' · ', identity] : os || identity) : null,
          time ? h('div', { style: styles.deviceMeta }, t('pairedAt', { time })) : null,
          error ? h('p', { style: styles.error, role: 'alert' }, t(error)) : null),
        revokeControls)
    }

    function ConnectionTestResult({ t, result, connected }) {
      if (result.serviceFailed) {
        return h('p', { style: styles.error, role: 'status', 'aria-live': 'polite' }, t(result.serviceUnavailable ? 'testServiceUnavailable' : 'testServiceFailure'))
      }
      if (['busy', 'rate_limited'].includes(result.configuration?.code)) {
        return h('p', { style: styles.hint, role: 'status' }, t('testRateLimited'))
      }
      if (result.mqtt?.code === 'credentials') {
        return h('p', { style: styles.error, role: 'status' }, t('testCredentials'))
      }
      const checks = [result.configuration, result.access, result.mqtt]
      const failed = checks.find((check) => check?.ok !== true)
      const passed = checks.every((check) => check?.ok === true)
      // Older Hosts returned a successful TCP probe without a broker login.
      if (passed && result.mqtt.authenticated !== true) {
        return h('p', { style: styles.error, role: 'status' }, t('testServiceUnavailable'))
      }
      const codes = new Set(['invalid_config', 'timeout', 'dns', 'refused', 'tls', 'not_ready', 'network', 'busy'])
      const reason = t(`test_${codes.has(failed?.code) ? failed.code : 'network'}`)
      const stage = t(result.configuration?.ok !== true ? 'testStageConfiguration'
        : result.access?.ok !== true ? 'testStageAccess' : 'testStageMqtt')
      const failureKey = result.running === 'connected' && connected ? 'testFailureOnline'
        : result.running === 'not_checked' ? 'testDraftFailure' : 'testFailure'
      return h('p', { style: passed ? styles.success : styles.error, role: 'status', 'aria-live': 'polite' },
        passed ? t('testSuccess') : t(failureKey, { stage, reason }))
    }

    function RuntimeDiagnostics({ t, snapshot }) {
      const diagnostics = snapshot.available ? snapshot.diagnostics : null
      const flag = (value, yes, no) => typeof value === 'boolean' ? t(value ? yes : no) : '—'
      const count = (value) => Number.isSafeInteger(value) && value >= 0 ? String(value) : '—'
      const rows = [
        ['diagnosticHostId', snapshot.available ? snapshot.hostId || '—' : '—'],
        ['diagnosticDsh', flag(diagnostics?.dshAvailable, 'diagnosticReady', 'diagnosticUnavailable')],
        ['diagnosticIdentity', flag(diagnostics?.identityReady, 'diagnosticReady', 'diagnosticUnavailable')],
        ['diagnosticRevocations', flag(diagnostics?.revocationsPending, 'diagnosticPending', 'diagnosticSynced')],
        ['diagnosticReconnects', count(diagnostics?.reconnects)],
        ['diagnosticPublishErrors', count(diagnostics?.publishErrors)],
        ['diagnosticOutbox', count(diagnostics?.pendingCiphertext)],
        ['diagnosticUnknown', count(diagnostics?.unknownCommands)],
        ['diagnosticSnapshots', count(diagnostics?.snapshotRequired)],
      ]
      return h('div', { style: styles.form },
        h('p', { style: styles.hint }, t('diagnosticsHint')),
        h('dl', { style: { margin: 0 } }, rows.map(([label, value]) =>
          h('div', { key: label, style: styles.diagnosticRow, className: 'whalerein-diagnostic-row' },
            h('dt', { style: { color: 'var(--dsw-alias-label-secondary)' } }, t(label)),
            h('dd', { style: { margin: 0, overflowWrap: 'anywhere' } }, value)))))
    }

    function formatExpiry(t, expiresAt, locale) {
      if (!(expiresAt > 0)) return t('pairingUriUnknownExpiry')
      const time = formatTime(expiresAt, locale)
      return time ? t('pairingUriExpires', { time }) : t('pairingUriUnknownExpiry')
    }

    function ConfigurationError({ t, error }) {
      if (!error) return null
      const detail = error.field
        ? t('configurationInvalidDetail', { field: t('configurationInvalidField', { field: error.field }), reason: error.reason ? t('configurationInvalidReason', { reason: error.reason }) : '' })
        : t('configurationInvalidGeneric')
      return h('section', { style: styles.statusCard, role: 'alert', 'aria-labelledby': 'whalerein-config-error-title' },
        h('div', { style: styles.statusHeader },
          h('span', { style: { ...styles.statusIcon, color: 'var(--dsw-alias-state-error-primary)' }, 'aria-hidden': true },
            h(IconWarningOutlineRegular, { size: 20 })),
          h('div', { style: styles.statusText },
            h('h3', { id: 'whalerein-config-error-title', style: { ...styles.statusTitle, margin: 0 } }, t('configurationInvalidTitle')),
            h('p', { style: { ...styles.statusHint, margin: 0 } }, detail))))
    }

    function ConnectionCard({ t, snapshot, inviting, inviteError, pairingUri, pairingExpiresAt, pairingExpired, locale, copyFeedback, onGenerate, onCopy, onRefresh, titleId }) {
      const connected = snapshot.connected === true
      const statusColor = connected ? 'var(--dsw-alias-state-success-primary)' : 'var(--dsw-alias-state-error-primary)'
      return h('section', { style: styles.statusCard, 'aria-labelledby': titleId },
        h('div', { style: styles.connectionHeader },
          h('div', { style: styles.statusHeader },
            h('span', { style: { ...styles.statusIcon, color: statusColor }, 'aria-hidden': true },
              h(connected ? IconCheckCircleOutlineRegular : IconWarningOutlineRegular, { size: 20 })),
            h('div', { style: styles.statusText },
              h('h3', { id: titleId, style: { ...styles.statusTitle, margin: 0 } }, t(connected ? 'connected' : 'disconnected')),
              connected ? null : h('p', { style: { ...styles.statusHint, margin: 0 } }, t('disconnectedHint')))),
          snapshot.available ? h(Button, { variant: 'primary', size: 'md', disabled: inviting, onClick: onGenerate }, inviting ? t('generating') : t('generate'))
            : h(Button, { variant: 'outline', size: 'md', onClick: onRefresh }, t('refresh'))),
        !snapshot.available ? h('p', { style: styles.hint, role: 'status' }, t('statusUnavailable')) : null,
        inviteError ? h('p', { style: styles.error, role: 'alert' }, t(inviteError)) : null,
        connected && snapshot.pairingCode ? h('div', { style: styles.connectionIdentity },
          h('div', { style: styles.connectionCopy },
            h('p', { style: styles.codeLabel }, t('fingerprint')),
            h('p', { style: styles.hint }, t('fingerprintHint'))),
          h('code', { style: styles.fingerprint, 'aria-label': t('fingerprint') }, snapshot.pairingCode)) : null,
        pairingUri ? h('div', { style: styles.pairingDetails },
          h('div', { style: styles.connectionHeader },
            h('div', { style: styles.connectionCopy },
              h('p', { style: styles.codeLabel }, t('pairingUri')),
              h('p', { style: { ...styles.hint, ...(pairingExpired ? styles.error : {}) } },
                pairingExpired ? t('pairingUriExpired') : formatExpiry(t, pairingExpiresAt, locale))),
            h(Button, { variant: 'outline', size: 'sm', disabled: pairingExpired, onClick: onCopy }, t('copyUri'))),
          h('code', { style: styles.code, 'aria-label': t('pairingUri') }, pairingUri),
          copyFeedback ? h('p', { style: copyFeedback === 'uriCopied' ? styles.success : styles.error, role: 'status' }, t(copyFeedback)) : null) : null)
    }

    function draftErrors(draft, { storedServer, legacyMqttUrl }) {
      const errors = {}
      const server = String(draft.server ?? '').trim()
      if (server) {
        if (!/^[A-Za-z0-9.-]{1,253}$/.test(server) || server.startsWith('.') || server.startsWith('-') ||
            server.endsWith('.') || server.endsWith('-') || server.includes('..')) errors.server = 'errorServerInvalid'
      } else if (storedServer && !legacyMqttUrl) {
        errors.server = 'errorServerRequired'
      }
      for (const key of ['access_port', 'mqtt_port']) {
        const value = String(draft[key] ?? '').trim()
        if (value !== '' && (!/^\d+$/.test(value) || Number(value) < 1 || Number(value) > 65535)) errors[key] = 'errorPortInvalid'
      }
      const hostId = String(draft.host_id ?? '').trim()
      if (hostId && !/^[A-Za-z0-9_.:-]{1,128}$/.test(hostId)) errors.host_id = 'errorHostIdInvalid'
      if (!['debug', 'info', 'warn', 'error', 'silent'].includes(String(draft.log_level ?? 'info'))) errors.log_level = 'errorLogLevelInvalid'
      return errors
    }

    function confirmationHostIdChange(t) {
      try {
        if (typeof window !== 'undefined' && typeof window.confirm === 'function') return window.confirm(t('hostIdChange'))
      } catch {
        // A blocked dialog must not silently rename the host.
        return false
      }
      return true
    }

    function LegacySettingsPage({ t, form, status, developerTools, locale, invite, revoke, secrets, testConnection }) {
      const tr = typeof t === 'function' ? t : (key) => key
      const [snapshot, setSnapshot] = React.useState(() => form.getSnapshot())
      const [draft, setDraft] = React.useState(() => initialDraft(snapshot.value))
      const [, setSecretVersion] = React.useState(0)
      const [saving, setSaving] = React.useState(false)
      const [testing, setTesting] = React.useState(false)
      const [testResult, setTestResult] = React.useState(null)
      const testVersion = React.useRef(0)
      React.useEffect(() => () => { testVersion.current += 1 }, [])
      const [error, setError] = React.useState('')
      const [saved, setSaved] = React.useState(false)
      const [copyFeedback, setCopyFeedback] = React.useState('')
      const [inviting, setInviting] = React.useState(false)
      const [revokeTarget, setRevokeTarget] = React.useState('')
      const [revoking, setRevoking] = React.useState(false)
      const [revokeError, setRevokeError] = React.useState({ id: '', key: '' })
      const [revokeFocus, setRevokeFocus] = React.useState('')
      const [pairingUri, setPairingUri] = React.useState('')
      const [pairingExpiresAt, setPairingExpiresAt] = React.useState(0)
      const [inviteError, setInviteError] = React.useState('')
      const [fieldErrors, setFieldErrors] = React.useState({})
      const [externalChange, setExternalChange] = React.useState(false)
      const [, setExpiryTick] = React.useState(0)
      const inviteInFlight = React.useRef(false)
      const revokeInFlight = React.useRef(false)
      const draftRef = React.useRef(draft)
      const baselineRef = React.useRef(initialDraft(snapshot.value))
      draftRef.current = draft
      const [statusSnapshot, setStatusSnapshot] = React.useState(() => status.getSnapshot())
      const [development, setDevelopment] = React.useState(() => developerTools.getSnapshot() === true)
      const [activeLocale, setActiveLocale] = React.useState(() => locale.getSnapshot().active)
      const fieldId = React.useId()
      const [menuOpen, setMenuOpen] = React.useState(false)
      const [advancedOpen, setAdvancedOpen] = React.useState(false)
      const [diagnosticsOpen, setDiagnosticsOpen] = React.useState(false)
      const [configurationOpen, setConfigurationOpen] = React.useState(() => !status.getSnapshot().connected)

      React.useEffect(() => form.subscribe(() => setSnapshot(form.getSnapshot())), [form])
      React.useEffect(() => status.subscribe(() => setStatusSnapshot(status.getSnapshot())), [status])
      React.useEffect(() => developerTools.subscribe(() => setDevelopment(developerTools.getSnapshot() === true)), [developerTools])
      React.useEffect(() => locale.subscribe(() => setActiveLocale(locale.getSnapshot().active)), [locale])
      React.useEffect(() => secrets.subscribe(() => setSecretVersion((version) => version + 1)), [secrets])
      React.useEffect(() => {
        testVersion.current += 1
        setTestResult(null)
        let cancelled = false
        void secrets.refresh().then(() => {
          if (cancelled) return
          const nextBaseline = initialDraft(snapshot.value)
          const dirty = FIELDS.some((field) => draftRef.current[field.key] !== baselineRef.current[field.key])
          baselineRef.current = nextBaseline
          if (dirty) {
            setExternalChange(true)
          } else {
            setDraft(nextBaseline)
            setExternalChange(false)
            setFieldErrors({})
          }
        })
        return () => {
          cancelled = true
        }
      }, [snapshot.revision])
      const dirty = FIELDS.some((field) => draft[field.key] !== initialDraft(snapshot.value)[field.key])
      React.useEffect(() => {
        if (!dirty || typeof window === 'undefined' || typeof window.addEventListener !== 'function') return undefined
        const guard = (event) => {
          event.preventDefault()
          event.returnValue = ''
          return ''
        }
        window.addEventListener('beforeunload', guard)
        return () => window.removeEventListener('beforeunload', guard)
      }, [dirty])
      React.useEffect(() => {
        if (!pairingUri || !(pairingExpiresAt > 0)) return undefined
        const remaining = pairingExpiresAt - Date.now()
        if (remaining <= 0) return undefined
        const timer = setTimeout(() => setExpiryTick((value) => value + 1), Math.min(remaining, 2147483647))
        return () => clearTimeout(timer)
      }, [pairingUri, pairingExpiresAt])
      React.useEffect(() => {
        setConfigurationOpen(!statusSnapshot.connected)
      }, [statusSnapshot.connected])
      React.useEffect(() => {
        if (!revokeFocus) return
        if (typeof document === 'undefined' || typeof document.getElementById !== 'function') return
        const node = document.getElementById(`${fieldId}-revoke-${revokeFocus}`)
        const trigger = node?.querySelector?.('button') ?? node
        trigger?.focus?.()
      }, [revokeFocus, fieldId])

      if (snapshot.status === 'loading') {
        return h('div', { style: styles.page, className: 'whalerein-settings' }, h('style', null, css), h('h2', { style: styles.title }, tr('title')), h('p', { style: styles.intro }, tr('loading')))
      }
      if (snapshot.status !== 'ready') {
        return h('div', { style: styles.page, className: 'whalerein-settings' }, h('style', null, css), h('h2', { style: styles.title }, tr('title')), h('p', { style: styles.intro }, tr('unavailable')))
      }

      const setField = (key, value) => {
        testVersion.current += 1
        setTestResult(null)
        setSaved(false)
        setError('')
        setFieldErrors((previous) => {
          if (!(key in previous)) return previous
          const next = { ...previous }
          delete next[key]
          return next
        })
        setDraft((previous) => ({ ...previous, [key]: value }))
      }
      const runConnectionTest = async () => {
        if (testing || saving) return
        const version = ++testVersion.current
        setTesting(true)
        setTestResult(null)
        const serviceFailure = (error) => ({ serviceFailed: true, serviceUnavailable: [
          'gateway/method-unavailable', 'gateway/invocation-unavailable',
          'gateway/service-unavailable', 'gateway/definition-unavailable',
        ].includes(error?.code) })
        try {
          const result = await testConnection({ ...draft })
          await status.refresh()
          if (testVersion.current === version) setTestResult(result?.ok && result.value ? result.value : serviceFailure(result?.error))
        } catch (error) {
          if (testVersion.current === version) setTestResult(serviceFailure(error))
        } finally {
          setTesting(false)
        }
      }
      const pairingExpired = pairingUri !== '' && pairingExpiresAt > 0 && Date.now() >= pairingExpiresAt
      const copyUri = async () => {
        setCopyFeedback('')
        if (pairingExpired) return
        try {
          const copied = pairingUri && await writeClipboard(pairingUri)
          setCopyFeedback(copied ? 'uriCopied' : 'copyFailed')
        } catch {
          setCopyFeedback('copyFailed')
        }
      }
      const generateLink = async () => {
        // A double click must never mint two one-time invitations.
        if (inviteInFlight.current) return
        inviteInFlight.current = true
        setInviteError('')
        setPairingUri('')
        setPairingExpiresAt(0)
        setCopyFeedback('')
        setInviting(true)
        try {
          const result = await invite()
          const uri = result?.ok ? result.value?.pairingUri : ''
          if (typeof uri === 'string' && uri.startsWith('whalerein://pair?')) {
            const expiry = Number(result.value?.expiresAtUnixMs)
            setPairingUri(uri)
            setPairingExpiresAt(Number.isSafeInteger(expiry) && expiry > 0 ? expiry : 0)
            return
          }
          setInviteError(inviteErrorKey(result?.error?.code))
        } catch (caught) {
          setInviteError(inviteErrorKey(caught?.code))
        } finally {
          inviteInFlight.current = false
          setInviting(false)
        }
      }
      const confirmRevoke = async (deviceId) => {
        if (revokeInFlight.current) return
        revokeInFlight.current = true
        setRevokeError({ id: '', key: '' })
        setRevokeTarget(deviceId)
        setRevoking(true)
        try {
          const result = await revoke(deviceId)
          if (result?.ok) {
            setRevokeTarget('')
            await status.refresh()
            return
          }
          setRevokeError({ id: deviceId, key: revokeErrorKey(result?.error?.code) })
        } catch (caught) {
          setRevokeError({ id: deviceId, key: revokeErrorKey(caught?.code) })
        } finally {
          revokeInFlight.current = false
          setRevoking(false)
        }
      }
      const save = async () => {
        if (!snapshot.writable || testing || saving) return
        const storedServer = String(snapshot.user?.server ?? snapshot.base?.server ?? snapshot.value?.server ?? '').trim()
        const legacyMqttUrl = String(snapshot.user?.mqtt_url ?? snapshot.base?.mqtt_url ?? '').trim()
        const validation = draftErrors(draft, { storedServer, legacyMqttUrl })
        if (Object.keys(validation).length) {
          setFieldErrors(validation)
          setError('')
          setSaved(false)
          return
        }
        setFieldErrors({})
        const storedHostId = String(snapshot.user?.host_id ?? snapshot.base?.host_id ?? snapshot.value?.host_id ?? '').trim()
        const nextHostId = String(draft.host_id ?? '').trim()
        if (storedHostId && nextHostId && nextHostId !== storedHostId && !confirmationHostIdChange(tr)) return
        testVersion.current += 1
        setTestResult(null)
        setSaving(true)
        setError('')
        setSaved(false)
        const revision = snapshot.revision
        try {
          const baseline = initialDraft(snapshot.value)
          const ops = []
          for (const field of FIELDS) {
            const key = field.key
            if (SECRET_FIELDS.has(key) && draft[key] === '') continue
            if (key === 'host_id' && nextHostId === '') continue
            if (draft[key] === baseline[key]) continue
            ops.push(draft[key] === '' ? { op: 'unset', path: [key] } : { op: 'set', path: [key], value: draft[key] })
          }
          if (String(draft.server ?? '').trim() !== '') {
            // Saved server settings always clear legacy explicit endpoints.
            ops.push({ op: 'unset', path: ['mqtt_url'] }, { op: 'unset', path: ['access_url'] })
          }
          let okay
          if (typeof form.mutate === 'function') {
            okay = await form.mutate(ops, revision)
          } else {
            // Older DSH releases lack atomic namespace writes; retained only
            // for compatibility with a host that already exposes this page.
            for (const op of ops) {
              okay = op.op === 'unset' ? await form.unset(op.path[0]) : await form.set(op.path[0], op.value)
              if (!okay) break
            }
          }
          if (!okay) {
            const changed = form.getSnapshot().revision !== revision
            setError(tr(changed ? 'errorSaveConflict' : 'saveFailed'))
            return
          }
          setExternalChange(false)
          setSaved(true)
        } catch {
          setError(tr('saveFailed'))
        } finally {
          setSaving(false)
        }
      }

      const devices = Array.isArray(statusSnapshot.devices) ? statusSnapshot.devices : []
      const baseline = initialDraft(snapshot.value)
      const primaryFields = FIELDS.filter((field) => !field.advanced)
      const advancedFields = FIELDS.filter((field) => field.advanced)
      const configError = statusSnapshot.configError ?? null

      return h('div', { style: styles.page, className: 'whalerein-settings' },
        h('style', null, css),
        h('header', { style: styles.header },
          h('h2', { style: styles.title }, tr('title'))),
        h(ConfigurationError, { t: tr, error: configError }),
        h(ConnectionCard, {
          t: tr, snapshot: statusSnapshot, inviting, inviteError, pairingUri, pairingExpiresAt,
          pairingExpired, locale: activeLocale, copyFeedback,
          onGenerate: generateLink, onCopy: copyUri, onRefresh: () => status.refresh(),
          titleId: `${fieldId}-connection-title`,
        }),
        !configError && externalChange ? h('section', { style: styles.section, role: 'status' },
          h('p', { style: styles.hint }, tr('externalChange')),
          h('div', { style: styles.formActions },
            h(Button, {
              variant: 'outline', size: 'sm',
              onClick: () => {
                const next = initialDraft(snapshot.value)
                baselineRef.current = next
                setDraft(next)
                setExternalChange(false)
                setFieldErrors({})
                setSaved(false)
              },
            }, tr('loadSaved')))) : null,
        !configError ? h('section', { style: styles.section, 'aria-labelledby': 'whalerein-devices-title' },
          h('div', { style: styles.sectionHeader },
            h('h3', { id: 'whalerein-devices-title', style: styles.sectionTitle }, tr('devices')),
            h(Tag, { tone: 'quiet' }, tr('deviceCount', { count: devices.length }))),
          devices.length > 0
            ? h('ul', { style: styles.deviceList, 'aria-label': tr('devices') }, devices.map((device) => h(DeviceRow, {
              key: device.deviceId,
              device,
              t: tr,
              locale: activeLocale,
              confirming: revokeTarget === device.deviceId,
              revoking: revoking && revokeTarget === device.deviceId,
              busy: revoking,
              id: `${fieldId}-revoke-${device.deviceId}`,
              autoFocus: revokeFocus === device.deviceId,
              error: revokeError.id === device.deviceId ? revokeError.key : '',
              onAsk: () => { if (revoking) return; setRevokeFocus(''); setRevokeTarget(device.deviceId); setRevokeError({ id: '', key: '' }) },
              onCancel: () => { if (!revoking) { setRevokeTarget(''); setRevokeFocus(device.deviceId) } },
              onConfirm: () => confirmRevoke(device.deviceId),
            })))
            : h('p', { style: styles.hint }, tr('noDevices'))) : null,
        h('section', { style: styles.details },
          h(DisclosureRow, {
            title: tr('configuration'), icon: h(IconSettingsOutlineRegular, { size: 16 }),
            open: configurationOpen, expandable: true, expandOnRowClick: true,
            onToggle: () => { setMenuOpen(false); setConfigurationOpen((open) => !open) },
          }, h('div', { style: styles.form },
            h('p', { style: styles.hint }, tr('configurationHint')),
            !snapshot.writable ? h('p', { style: styles.hint }, tr('readOnly')) : null,
            h('div', null,
              primaryFields.map((field) => h(Field, { key: field.key, field, draft, t: tr, onChange: setField, secretSet: secrets.has(field.key), id: `${fieldId}-${field.key}`, disabled: saving || !snapshot.writable, error: fieldErrors[field.key] ? tr(fieldErrors[field.key]) : '' })),
              h(DisclosureRow, {
                title: tr('advanced'), icon: h(IconSlidersTwoOutlineRegular, { size: 16 }),
                open: advancedOpen, expandable: true, expandOnRowClick: true,
                onToggle: () => { setMenuOpen(false); setAdvancedOpen((open) => !open) },
              }, h('div', { style: styles.form },
                advancedFields.map((field) => h(Field, { key: field.key, field, draft, t: tr, onChange: setField, id: `${fieldId}-${field.key}`, menuOpen, setMenuOpen, disabled: saving || !snapshot.writable, error: fieldErrors[field.key] ? tr(fieldErrors[field.key]) : '' }))))),
            // The official form has a fixed footer; compose official buttons
            // here so saving and testing share one accessible action row.
            h('div', { style: styles.formActions },
              h(Button, { variant: 'primary', size: 'sm', disabled: saving || testing || !snapshot.writable || !dirty, onClick: save }, saving ? tr('saving') : tr('save')),
              h(Button, { variant: 'outline', size: 'sm', disabled: saving || testing, onClick: runConnectionTest }, testing ? tr('testingConnection') : tr('testConnection'))),
            testing ? h('p', { style: styles.hint, role: 'status' }, tr('testingConnection')) : null,
            testResult ? h(ConnectionTestResult, { t: tr, result: testResult, connected: statusSnapshot.connected }) : null,
            error ? h('p', { style: styles.error, role: 'alert' }, error) : null,
            saved ? h('p', { style: styles.success, role: 'status' }, tr('saved')) : null))),
        development ? h('section', { style: styles.details },
          h(DisclosureRow, {
            title: tr('diagnostics'), icon: h(IconCodeOutlineRegular, { size: 16 }),
            headerAccessory: h(Tag, { tone: 'quiet' }, tr('debugOnly')),
            open: diagnosticsOpen, expandable: true, expandOnRowClick: true,
            onToggle: () => setDiagnosticsOpen((open) => !open),
          },
          h(RuntimeDiagnostics, { t: tr, snapshot: statusSnapshot }))) : null)
    }

    const MANAGED_STATES = new Set([
      'idle', 'requesting', 'awaiting_approval', 'enrolling', 'recovering',
      'connecting', 'connected', 'reconnecting', 'disconnected', 'denied',
      'expired', 'action_required', 'removing', 'removed',
    ])
    const TRANSITIONAL_STATES = new Set(['requesting', 'enrolling', 'recovering', 'connecting', 'removing'])

    function safeApprovalUrl(value) {
      if (typeof value !== 'string' || value.length === 0 || value.length > 2048) return ''
      try {
        const url = new URL(value)
        if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) return ''
        return url.toString()
      } catch {
        return ''
      }
    }

    function localeApprovalUrl(value, active) {
      const url = safeApprovalUrl(value)
      if (!url) return ''
      try {
        const parsed = new URL(url)
        if (!parsed.searchParams.has('locale')) parsed.searchParams.set('locale', active === 'zh' ? 'zh-CN' : 'en')
        return parsed.toString()
      } catch {
        return ''
      }
    }

    function reserveBrowserTab() {
      if (typeof window === 'undefined' || typeof window.open !== 'function') return null
      try {
        // Reserve synchronously during the click so a popup blocker evaluates
        // against the explicit user action, before the RPC resolves.
        return window.open('', '_blank') ?? null
      } catch {
        return null
      }
    }

    function commitBrowserTab(tab, url) {
      if (!tab) return false
      try {
        tab.opener = null
        if (tab.location) {
          tab.location.href = url
        } else if (typeof tab.location?.assign === 'function') {
          tab.location.assign(url)
        } else {
          return false
        }
        return true
      } catch {
        try {
          tab.location.assign?.(url)
          return true
        } catch {
          return false
        }
      }
    }

    function createConnectionActions(remote) {
      const invoke = async (method, ...args) => {
        const fn = remote?.whalerein?.[method]
        if (typeof fn !== 'function') return { ok: false, code: 'gateway/method-unavailable' }
        try {
          const result = await fn(...args)
          if (result?.ok === false) return { ok: false, code: result.error?.code ?? 'gateway/internal' }
          return { ok: true, value: result?.value ?? null }
        } catch (error) {
          return { ok: false, code: typeof error?.code === 'string' ? error.code : 'gateway/internal' }
        }
      }
      return {
        invoke,
        startConnect: (request) => invoke('startConnect', request),
        cancel: () => invoke('cancel'),
        retry: () => invoke('retry'),
        disconnect: () => invoke('disconnect'),
        reconnect: () => invoke('reconnect'),
        removeComputer: () => invoke('removeComputer'),
        switchProfile: (id) => invoke('switchProfile', id),
        enableStaging: () => invoke('enableStaging'),
        addCustomService: (value) => invoke('addCustomService', value),
        deleteProfile: (id) => invoke('deleteProfile', id),
      }
    }

    function managedEnvironmentLabel(t, environment) {
      if (environment === 'local') return t('cloudLocal')
      if (environment === 'staging') return t('cloudStagingActive')
      if (environment === 'custom') return t('cloudCustom')
      if (environment === 'standalone') return t('cloudLocalOnly')
      return t('cloudProduction')
    }

    function ProfileRows({ t, profiles, activeId, pending, busy, onDelete }) {
      if (!Array.isArray(profiles) || profiles.length === 0) {
        return h('p', { style: styles.hint }, t('cloudNoProfiles'))
      }
      return h('ul', { style: styles.deviceList, 'aria-label': t('cloudProfiles') },
        profiles.map((profile) => {
          const active = profile.active === true || profile.id === activeId
          const disabled = busy || pending || active || profile.managed === false
          return h('li', { key: profile.id, style: styles.profileRow, className: 'whalerein-profile-row' },
            h('div', { style: styles.profileCopy },
              h('div', { style: styles.profileName }, profile.label || profile.id),
              h('div', { style: styles.deviceMeta },
                managedEnvironmentLabel(t, profile.environment),
                active ? ` · ${t('cloudDeleteActive')}` : '')),
            h(Button, {
              variant: 'ghost', size: 'sm', style: styles.dangerAction,
              disabled, 'aria-label': `${t('cloudDeleteProfile')}: ${profile.label || profile.id}`,
              onClick: () => onDelete(profile.id),
            }, t('cloudDeleteProfile')))
        }))
    }

    function ManagedSettingsPage({ t, status, connection, developerTools, locale: localeService }) {
      const tr = typeof t === 'function' ? t : (key) => key
      const [snapshot, setSnapshot] = React.useState(() => status.getSnapshot())
      const [busy, setBusy] = React.useState('')
      const [actionError, setActionError] = React.useState('')
      const [approvalLink, setApprovalLink] = React.useState('')
      const [removalLink, setRemovalLink] = React.useState('')
      const [browserBlocked, setBrowserBlocked] = React.useState(false)
      const [detailsOpen, setDetailsOpen] = React.useState(false)
      const [advancedOpen, setAdvancedOpen] = React.useState(false)
      const [developerOpen, setDeveloperOpen] = React.useState(false)
      const [details, setDetails] = React.useState(null)
      const [detailsLoading, setDetailsLoading] = React.useState(false)
      const [customLabel, setCustomLabel] = React.useState('')
      const [customOrigin, setCustomOrigin] = React.useState('')
      const [customNotice, setCustomNotice] = React.useState('')
      const [profileNotice, setProfileNotice] = React.useState('')
      const [development, setDevelopment] = React.useState(() => developerTools?.getSnapshot?.() === true)
      const [stagingFallbackId, setStagingFallbackId] = React.useState('')
      const [productionFallbackId, setProductionFallbackId] = React.useState('')
      const fieldId = React.useId()
      const startingRef = React.useRef(false)
      const lastState = React.useRef(snapshot.state)
      React.useEffect(() => status.subscribe(() => {
        const next = status.getSnapshot()
        setSnapshot(next)
      }), [status])
      React.useEffect(() => developerTools?.subscribe?.(() => setDevelopment(developerTools.getSnapshot() === true)), [developerTools])
      React.useEffect(() => {
        if (snapshot.environment === 'production' && snapshot.profileId) setProductionFallbackId(snapshot.profileId)
        if (snapshot.environment === 'staging' && snapshot.profileId) setStagingFallbackId(snapshot.profileId)
      }, [snapshot.environment, snapshot.profileId])
      React.useEffect(() => {
        if (lastState.current === snapshot.state) return
        lastState.current = snapshot.state
        // Move focus to the announced status so keyboard and screen-reader
        // users learn about the state change without losing their place.
        if (typeof document !== 'undefined' && typeof document.getElementById === 'function') {
          document.getElementById(`${fieldId}-state`)?.focus?.()
        }
      }, [snapshot.state, fieldId])
      React.useEffect(() => {
        if (!detailsOpen || typeof connection?.invoke !== 'function') return undefined
        let cancelled = false
        setDetailsLoading(true)
        void connection.invoke('details').then((result) => {
          if (cancelled) return
          setDetails(result.ok && result.value && typeof result.value === 'object' ? result.value : null)
        }).finally(() => {
          if (!cancelled) setDetailsLoading(false)
        })
        return () => { cancelled = true }
      }, [detailsOpen, connection, snapshot.state])

      if (snapshot.ready !== true) {
        return h('div', { style: styles.page, className: 'whalerein-settings' },
          h('style', null, css),
          h('h2', { style: styles.title }, tr('title')),
          h('p', { style: styles.intro }, tr('loading')))
      }

      const state = MANAGED_STATES.has(snapshot.state) ? snapshot.state : 'action_required'
      const transitional = TRANSITIONAL_STATES.has(state)
      const pending = snapshot.pendingOperation === true
      const activeProfile = (snapshot.profiles ?? []).find((profile) => profile.active === true)
      const isStaging = snapshot.environment === 'staging' || activeProfile?.environment === 'staging'
      const stagingProfile = (snapshot.profiles ?? []).find((profile) => profile.environment === 'staging')
      const productionProfile = (snapshot.profiles ?? []).find((profile) => profile.environment === 'production')
      // The frozen runtime snapshot may not list profiles yet. Fall back to the
      // opaque ids the manager returns for the active profile and for a newly
      // added staging profile, so switching stays usable either way.
      const stagingProfileId = stagingProfile?.id ?? stagingFallbackId
      const productionProfileId = productionProfile?.id ?? productionFallbackId
      const activeLocale = localeService?.getSnapshot?.().active
      const approvalUrl = localeApprovalUrl(approvalLink || snapshot.approvalUrl, activeLocale)
      const showConnect = ['idle', 'disconnected'].includes(state)
      const showCancel = ['requesting', 'awaiting_approval', 'enrolling', 'recovering'].includes(state)
      const showRetry = ['denied', 'expired', 'action_required'].includes(state)
      const showDisconnect = state === 'connected' || state === 'reconnecting'
      const showDeveloperTools = development || snapshot.developerToolsEnabled === true

      const run = async (method, label, ...args) => {
        if (busy || typeof connection?.invoke !== 'function') return
        setBusy(label)
        setActionError('')
        setProfileNotice('')
        const result = await connection.invoke(method, ...args)
        if (!result.ok) setActionError('cloudActionFailed')
        await status.refresh?.()
        setBusy('')
      }

      const start = async () => {
        if (startingRef.current || busy || typeof connection?.invoke !== 'function') return
        startingRef.current = true
        setBusy('start')
        setActionError('')
        setBrowserBlocked(false)
        setApprovalLink('')
        // Reserve the tab during the explicit click, before awaiting the RPC.
        const tab = reserveBrowserTab()
        try {
          const result = await connection.invoke('startConnect', { source: 'settings' })
          await status.refresh?.()
          if (!result.ok) {
            try { tab?.close?.() } catch { /* already closed */ }
            setActionError('cloudActionFailed')
            return
          }
          const active = localeService?.getSnapshot?.().active
          const url = localeApprovalUrl(result.value?.approvalUrl, active)
            || localeApprovalUrl(result.value?.snapshot?.approvalUrl, active)
            || localeApprovalUrl(status.getSnapshot().approvalUrl, active)
          if (url) {
            const opened = commitBrowserTab(tab, url)
            setApprovalLink(url)
            setBrowserBlocked(!opened)
          } else {
            try { tab?.close?.() } catch { /* already closed */ }
          }
        } finally {
          startingRef.current = false
          setBusy('')
        }
      }

      const remove = async () => {
        if (busy || pending || typeof connection?.invoke !== 'function') return
        setBusy('remove')
        setActionError('')
        const tab = reserveBrowserTab()
        const result = await connection.invoke('removeComputer')
        await status.refresh?.()
        if (!result.ok) {
          try { tab?.close?.() } catch { /* already closed */ }
          setActionError('cloudActionFailed')
          setBusy('')
          return
        }
        const active = localeService?.getSnapshot?.().active
        const url = localeApprovalUrl(result.value?.action?.url, active)
        if (url) {
          const opened = commitBrowserTab(tab, url)
          setRemovalLink(url)
          setBrowserBlocked(!opened)
        } else {
          try { tab?.close?.() } catch { /* already closed */ }
        }
        setBusy('')
      }
      const enableStaging = async () => {
        if (busy || typeof connection?.invoke !== 'function') return
        setBusy('staging')
        setActionError('')
        const result = await connection.invoke('enableStaging')
        if (!result.ok) {
          setActionError('cloudActionFailed')
        } else if (result.value?.action?.id) {
          setStagingFallbackId(result.value.action.id)
        }
        await status.refresh?.()
        setBusy('')
      }
      const configuration = snapshot.configuration ?? null
      const configurationRows = configuration ? [
        [tr('cloudDetailsDesired'), configuration.desiredDefault
          ? managedEnvironmentLabel(tr, configuration.desiredDefault) : '—'],
        [tr('cloudDetailsSource'), configuration.definitionSource === 'builtin'
          ? tr('cloudSourceBuiltin')
          : configuration.definitionSource === 'file' ? tr('cloudSourceFile') : '—'],
      ] : []
      const entries = [
        [tr('cloudDetailsState'), tr(`state_${state}`)],
        [tr('cloudDetailsEnvironment'), managedEnvironmentLabel(tr, snapshot.environment)],
        ...configurationRows,
        [tr('cloudDetailsHostName'), snapshot.hostName || '—'],
        [tr('cloudDetailsProfile'), snapshot.label || '—'],
      ]
      const detailRows = details ? [
        [tr('cloudDetailsState'), details.state ? tr(`state_${details.state}`) : '—'],
        [tr('cloudDetailsEnvironment'), managedEnvironmentLabel(tr, details.environment || snapshot.environment)],
        [tr('cloudDetailsRegion'), details.region || '—'],
        [tr('cloudDetailsOrigin'), details.accessOrigin || '—'],
        [tr('cloudDetailsRevision'), details.revision == null ? '—' : String(details.revision)],
        [tr('cloudDetailsLastError'), details.lastErrorCode || '—'],
      ] : entries

      return h('div', { style: styles.page, className: 'whalerein-settings' },
        h('style', null, css),
        h('header', { style: styles.header },
          h('div', { style: styles.stagingLabel },
            h('h2', { style: styles.title }, tr('title')),
            isStaging ? h(Tag, { tone: 'neutral', 'data-whalerein-staging': 'true' }, tr('cloudStagingActive'))
              : snapshot.environment === 'local' ? h(Tag, { tone: 'neutral', 'data-whalerein-local': 'true' }, tr('cloudLocal')) : null),
          h('p', { style: styles.intro }, tr('cloudIntro'))),
        isStaging && !development
          ? h('div', { style: styles.managedActions, className: 'whalerein-managed-actions' },
            h(Tag, { tone: 'neutral', 'data-whalerein-staging': 'true' }, tr('cloudStagingActive')),
            h(Button, {
              variant: 'outline', size: 'sm', disabled: !!busy,
              onClick: () => run('switchProfile', 'switch', productionProfileId ?? 'production'),
            }, busy === 'switch' ? tr('cloudSwitching') : tr('cloudSwitchProduction')))
          : null,
        h('section', { style: styles.statusCard, 'aria-labelledby': `${fieldId}-state-title` },
          h('div', { style: styles.connectionHeader },
            h('div', { style: styles.statusHeader },
              h('span', { style: { ...styles.statusIcon, color: state === 'connected' ? 'var(--dsw-alias-state-success-primary)' : 'var(--dsw-alias-label-secondary)' }, 'aria-hidden': true },
                h(state === 'connected' ? IconCheckCircleOutlineRegular : IconWarningOutlineRegular, { size: 20 })),
              h('div', {
                style: styles.managedStateRegion,
                id: `${fieldId}-state`,
                role: 'status', 'aria-live': state === 'connected' ? 'polite' : 'assertive',
                'aria-atomic': 'true', tabIndex: -1,
              },
                h('h3', { id: `${fieldId}-state-title`, style: { ...styles.statusTitle, margin: 0 } }, tr(`state_${state}`)),
                h('p', { id: `${fieldId}-state-hint`, style: { ...styles.statusHint, margin: 0 } }, tr(`stateHint_${state}`)))),
            h('div', { style: styles.managedActions, className: 'whalerein-managed-actions' },
              showConnect ? h(Button, {
                variant: 'primary', size: 'md', disabled: !!busy || pending,
                'aria-describedby': `${fieldId}-state-hint`, onClick: start,
              }, busy === 'start' ? tr('state_requesting') : state === 'idle' ? tr('cloudConnect') : tr('cloudTryAgain')) : null,
              showRetry ? h(Button, {
                variant: 'primary', size: 'md', disabled: !!busy || pending,
                onClick: () => run('retry', 'retry'),
              }, busy === 'retry' ? tr('state_requesting') : tr('cloudTryAgain')) : null,
              showCancel ? h(Button, {
                variant: 'outline', size: 'md', disabled: !!busy,
                onClick: () => run('cancel', 'cancel'),
              }, busy === 'cancel' ? tr('cloudCancelling') : tr('cloudCancel')) : null,
              showDisconnect ? h(Button, {
                variant: 'outline', size: 'md', disabled: !!busy,
                onClick: () => run('disconnect', 'disconnect'),
              }, busy === 'disconnect' ? tr('cloudDisconnecting') : tr('cloudDisconnect')) : null,
              )),
          !snapshot.available ? h('p', { style: styles.hint, role: 'status' }, tr('cloudStatusUnavailable')) : null,
          pending ? h('p', { style: styles.hint, role: 'status' }, tr('cloudPending')) : null,
          ['requesting', 'awaiting_approval'].includes(state) && approvalUrl
            ? h('div', { style: styles.managedActions, className: 'whalerein-managed-actions' },
              h('a', {
                href: approvalUrl, target: '_blank', rel: 'noopener noreferrer',
                style: styles.approvalLink, className: 'whalerein-approval-link',
              }, tr('cloudOpenBrowser')),
              browserBlocked ? h('p', { style: styles.hint, role: 'status' }, tr('cloudBrowserBlocked')) : null)
            : null,
          state === 'removing'
            ? h('div', { style: styles.managedActions, className: 'whalerein-managed-actions' },
              h('p', { style: styles.hint, role: 'status' }, tr('cloudConfirmationRequired')),
              removalLink ? h('a', {
                href: removalLink, target: '_blank', rel: 'noopener noreferrer',
                style: styles.approvalLink, className: 'whalerein-approval-link',
              }, tr('cloudOpenBrowser')) : null,
              browserBlocked ? h('p', { style: styles.hint, role: 'status' }, tr('cloudBrowserBlocked')) : null)
            : null,
          actionError ? h('p', { style: styles.error, role: 'alert' }, tr(actionError)) : null),
        h('section', { style: styles.details },
          h(DisclosureRow, {
            title: tr('cloudDetails'), icon: h(IconSettingsOutlineRegular, { size: 16 }),
            headerAccessory: isStaging ? h(Tag, { tone: 'neutral' }, tr('cloudStagingActive')) : null,
            open: detailsOpen, expandable: true, expandOnRowClick: true,
            onToggle: () => setDetailsOpen((open) => !open),
          }, h('div', { style: styles.form },
            h('p', { style: styles.hint }, tr('cloudDetailsHint')),
            h('dl', { style: { margin: 0 } }, detailRows.map(([label, value], index) =>
              h('div', { key: `${label}-${index}`, style: styles.diagnosticRow, className: 'whalerein-diagnostic-row' },
                h('dt', { style: { color: 'var(--dsw-alias-label-secondary)' } }, label),
                h('dd', { style: styles.detailValue }, value)))),
            detailsOpen ? h('div', { style: styles.managedActions },
              h(Button, {
                variant: 'ghost', size: 'sm', disabled: detailsLoading,
                onClick: () => {
                  setDetailsLoading(true)
                  void connection.invoke('details').then((result) => {
                    setDetails(result.ok && result.value && typeof result.value === 'object' ? result.value : null)
                  }).finally(() => setDetailsLoading(false))
                },
              }, detailsLoading ? tr('loading') : tr('cloudDetailsRefresh'))) : null,
            h('p', { style: styles.hint }, tr('cloudDisconnectHint')),
            h('p', { style: styles.hint }, tr('cloudRemoveHint')),
            h('div', { style: styles.managedActions },
              showDisconnect ? h(Button, {
                variant: 'outline', size: 'sm', disabled: !!busy,
                onClick: () => run('disconnect', 'disconnect'),
              }, tr('cloudDisconnect')) : null,
              h(Button, {
                variant: 'ghost', size: 'sm', style: styles.dangerAction,
                disabled: !!busy || pending || state === 'removing' || state === 'removed',
                onClick: remove,
              }, busy === 'remove' ? tr('cloudRemoving') : tr('cloudRemove'))),
            h('p', { style: styles.hint }, tr('cloudPairingManaged'))))),
        h('section', { style: styles.details },
          h(DisclosureRow, {
            title: tr('cloudAdvanced'), icon: h(IconSlidersTwoOutlineRegular, { size: 16 }),
            open: advancedOpen, expandable: true, expandOnRowClick: true,
            onToggle: () => setAdvancedOpen((open) => !open),
          }, h('div', { style: styles.form },
            h('h4', { style: styles.codeLabel }, tr('cloudCustomService')),
            h('p', { style: styles.hint }, tr('cloudCustomServiceHint')),
            h('div', null,
              h(Field, {
                field: { key: 'custom_label', label: 'cloudCustomLabel', subtitle: 'cloudCustomServiceHint', kind: 'text' },
                draft: { custom_label: customLabel }, t: tr,
                onChange: (_key, value) => setCustomLabel(value),
                id: `${fieldId}-custom-label`, disabled: !!busy, error: '',
              }),
              h(Field, {
                field: { key: 'custom_origin', label: 'cloudCustomOrigin', subtitle: 'cloudCustomServiceHint', placeholder: 'cloudCustomOriginPlaceholder', kind: 'text' },
                draft: { custom_origin: customOrigin }, t: tr,
                onChange: (_key, value) => setCustomOrigin(value),
                id: `${fieldId}-custom-origin`, disabled: !!busy, error: '',
              })),
            h('div', { style: styles.managedActions },
              h(Button, {
                variant: 'outline', size: 'sm', disabled: !!busy || customLabel.trim() === '' || customOrigin.trim() === '',
                onClick: async () => {
                  if (busy) return
                  setBusy('custom')
                  setActionError('')
                  setCustomNotice('')
                  const result = await connection.invoke('addCustomService', { label: customLabel.trim(), apiOrigin: customOrigin.trim() })
                  if (result.ok) {
                    setCustomNotice('cloudCustomServiceAdded')
                    setCustomLabel('')
                    setCustomOrigin('')
                  } else {
                    setActionError('cloudActionFailed')
                  }
                  await status.refresh?.()
                  setBusy('')
                },
              }, busy === 'custom' ? tr('saving') : tr('cloudAddCustomService'))),
            customNotice ? h('p', { style: styles.success, role: 'status' }, tr(customNotice)) : null,
            h('h4', { style: styles.codeLabel }, tr('cloudProfiles')),
            h('p', { style: styles.hint }, tr('cloudProfilesHint')),
            h(ProfileRows, {
              t: tr, profiles: snapshot.profiles, activeId: snapshot.profileId, pending,
              busy: !!busy,
              onDelete: async (id) => {
                if (busy) return
                setBusy('delete')
                setActionError('')
                setProfileNotice('')
                const result = await connection.invoke('deleteProfile', id)
                if (result.ok) setProfileNotice('cloudRemovedLocal')
                else setActionError('cloudActionFailed')
                await status.refresh?.()
                setBusy('')
              },
            }),
            h('p', { style: styles.hint }, tr('cloudDeleteHint')),
            profileNotice ? h('p', { style: styles.success, role: 'status' }, tr(profileNotice)) : null))),
        showDeveloperTools
          ? h(DeveloperToolsSection, {
            t: tr, snapshot, isStaging, stagingProfileId, productionProfileId, busy, developerOpen,
            setDeveloperOpen, onEnableStaging: enableStaging,
            onSwitch: (id) => run('switchProfile', 'switch', id),
          })
          : null)
    }

    function DeveloperToolsSection({ t, snapshot, isStaging, stagingProfileId, productionProfileId, busy, developerOpen, setDeveloperOpen, onEnableStaging, onSwitch }) {
      return h('section', { style: styles.details },
        h(DisclosureRow, {
          title: t('cloudDeveloperTools'), icon: h(IconCodeOutlineRegular, { size: 16 }),
          headerAccessory: h(Tag, { tone: 'quiet' }, t('debugOnly')),
          open: developerOpen, expandable: true, expandOnRowClick: true,
          onToggle: () => setDeveloperOpen((open) => !open),
        }, h('div', { style: styles.form },
          h('p', { style: styles.hint }, t('cloudDeveloperToolsHint')),
          ...(snapshot.profiles ?? []).filter((p) => p.environment === 'local').map((p) =>
            h(Button, { key: p.id, variant: 'outline', size: 'sm', disabled: !!busy || p.active,
              onClick: () => onSwitch(p.id) }, t('cloudUseLocal'))),
          !snapshot.stagingEnabled
            ? h(Button, {
              variant: 'outline', size: 'sm', disabled: !!busy,
              onClick: onEnableStaging,
            }, busy === 'staging' ? t('cloudEnablingStaging') : t('cloudEnableStaging'))
            : h('div', null,
              h('p', { style: styles.hint }, t('cloudStagingNote')),
              h('div', { style: styles.profileRow, className: 'whalerein-profile-row' },
                h('div', { style: styles.profileCopy },
                  h('div', { style: styles.profileName }, t('cloudStagingProfile')),
                  h('div', { style: styles.deviceMeta }, isStaging ? t('cloudStagingActive') : t('cloudProduction'))),
                isStaging
                  ? h(Button, {
                    variant: 'outline', size: 'sm', disabled: !!busy,
                    onClick: () => onSwitch(productionProfileId ?? 'production'),
                  }, busy === 'switch' ? t('cloudSwitching') : t('cloudSwitchProduction'))
                  : h(Button, {
                    variant: 'primary', size: 'sm', disabled: !!busy || !stagingProfileId,
                    onClick: () => onSwitch(stagingProfileId),
                  }, busy === 'switch' ? t('cloudSwitching') : t('cloudUseStaging')))))))
    }

    function SettingsPage(props) {
      const [ready, setReady] = React.useState(() => props.status.getSnapshot().ready === true)
      const [managed, setManaged] = React.useState(() => props.status.getSnapshot().managed === true)
      React.useEffect(() => props.status.subscribe(() => {
        const snapshot = props.status.getSnapshot()
        setReady(snapshot.ready === true)
        setManaged(snapshot.managed === true)
      }), [props.status])
      if (!ready) {
        return h('div', { style: styles.page, className: 'whalerein-settings' },
          h('style', null, css),
          h('h2', { style: styles.title }, (props.t ?? ((key) => key))('title')),
          h('p', { style: styles.intro }, (props.t ?? ((key) => key))('loading')))
      }
      return managed ? h(ManagedSettingsPage, props) : h(LegacySettingsPage, props)
    }

    function createSecretPresence(remote) {
      let keys = new Set()
      const listeners = new Set()
      const refresh = async () => {
        let next = new Set()
        try {
          const result = await remote.settings.describe()
          const namespaces = result?.ok ? result.value?.namespaces : undefined
          const row = Array.isArray(namespaces) ? namespaces.find((item) => item?.ns === ENTRY) : undefined
          for (const secret of row?.secrets ?? []) {
            if (secret?.set !== true || !Array.isArray(secret.path) || secret.path.length !== 1) continue
            next.add(secret.path[0])
          }
        } catch {
          next = new Set()
        }
        const same = next.size === keys.size && [...next].every((key) => keys.has(key))
        keys = next
        if (!same) {
          for (const listener of listeners) listener()
        }
      }
      return {
        has: (key) => keys.has(key),
        subscribe(listener) {
          listeners.add(listener)
          return () => listeners.delete(listener)
        },
        refresh,
      }
    }

    function cloudSnapshot(value) {
      const text = (item, max = 200) => typeof item === 'string'
        ? item.replace(/[\u0000-\u001f\u007f]/g, ' ').slice(0, max) : ''
      return {
        available: value.available === true,
        managed: value.managed === true,
        state: typeof value.state === 'string' ? value.state : 'action_required',
        environment: ['production', 'staging', 'local', 'custom', 'standalone'].includes(value.environment)
          ? value.environment : 'production',
        profileId: text(value.profileId, 128),
        label: text(value.label, 64),
        hostName: text(value.hostName, 64),
        stagingEnabled: value.stagingEnabled === true,
        developerToolsEnabled: value.developerToolsEnabled === true,
        approvalUrl: safeApprovalUrl(value.approvalUrl),
        error: value.error && typeof value.error === 'object' ? { code: text(value.error.code, 64) } : null,
        pendingOperation: value.pendingOperation === true,
        profiles: Array.isArray(value.profiles) ? value.profiles.slice(0, 32) : [],
        capabilities: Array.isArray(value.capabilities) ? value.capabilities.slice(0, 8) : [],
        configuration: value.configuration && typeof value.configuration === 'object'
          ? {
              sourceKind: text(value.configuration.sourceKind, 16),
              selectedEnvironment: text(value.configuration.selectedEnvironment, 64),
              desiredDefault: text(value.configuration.desiredDefault, 64),
              definitionSource: text(value.configuration.definitionSource, 16),
              selectionSource: text(value.configuration.selectionSource, 16),
            }
          : null,
      }
    }

    function createStatusStore(remote) {
      let snapshot = { available: false, ready: false, managed: false, connected: false, hostId: '', pairingCode: '', devices: [] }
      let timer = null
      let disposed = false
      let running = false
      let queued = false
      let refreshPromise = null
      const listeners = new Set()
      const publish = (next) => {
        snapshot = next
        for (const listener of listeners) listener()
      }
      const refresh = () => {
        if (disposed) return Promise.resolve()
        if (running) {
          queued = true
          return refreshPromise
        }
        running = true
        refreshPromise = (async () => {
          try {
            const result = await remote.whalerein.snapshot()
            if (disposed) return
            if (result?.ok && result.value && typeof result.value === 'object') {
              publish({
                ready: true,
                available: result.value.available === true,
                connected: result.value.connected === true,
                hostId: typeof result.value.hostId === 'string' ? result.value.hostId : '',
                pairingCode: typeof result.value.pairingCode === 'string' ? result.value.pairingCode : '',
                devices: Array.isArray(result.value.devices) ? result.value.devices : [],
                diagnostics: result.value.diagnostics ?? null,
                configError: result.value.configError ?? null,
                ...cloudSnapshot(result.value),
              })
            } else {
              // A transient read failure must not look like every device was
              // unpaired; keep the last known list and mark status unavailable.
              publish({ ...snapshot, ready: true, available: false })
            }
          } catch {
            if (!disposed) publish({ ...snapshot, ready: true, available: false })
          } finally {
            running = false
            if (queued && !disposed) {
              queued = false
              await refresh()
            }
          }
        })()
        return refreshPromise
      }
      return {
        getSnapshot: () => snapshot,
        subscribe(listener) {
          listeners.add(listener)
          if (listeners.size === 1) {
            void refresh()
            timer = setInterval(() => void refresh(), 3000)
          }
          return () => {
            listeners.delete(listener)
            if (listeners.size === 0 && timer !== null) {
              clearInterval(timer)
              timer = null
            }
          }
        },
        refresh,
        async dispose() {
          disposed = true
          if (timer !== null) clearInterval(timer)
          timer = null
          listeners.clear()
          await refreshPromise
        },
      }
    }

    const jsonCodec = (typeSymbol) => ({
      mode: 'strict', typeSymbol: `whalerein-dsh#${typeSymbol}`, create: () => ({ parse: (value) => value }),
    })
    // Method names must avoid DSH's RemoteNamespaceService fields and prototype
    // members (ctx, empty, name, methods, namespace, has, install, installDirect,
    // installScoped, remove): the gateway rejects the whole mount on a collision.
    // `remove` is reserved, so the computer-removal action is `removeComputer`.
    const MANAGER_REMOTE_DESCRIPTORS = [
      ['details', [], 'WhaleReinConnectionDetails'],
      ['startConnect', [{ name: 'request', wire: 'request', source: 'json', codec: jsonCodec('ConnectionStartRequest') }], 'WhaleReinConnectionSnapshot'],
      ['cancel', [], 'WhaleReinConnectionSnapshot'],
      ['retry', [], 'WhaleReinConnectionSnapshot'],
      ['disconnect', [], 'WhaleReinConnectionSnapshot'],
      ['reconnect', [], 'WhaleReinConnectionSnapshot'],
      ['removeComputer', [], 'WhaleReinConnectionSnapshot'],
      ['switchProfile', [{ name: 'id', wire: 'id', source: 'json', codec: jsonCodec('ProfileId') }], 'WhaleReinConnectionSnapshot'],
      ['enableStaging', [], 'WhaleReinConnectionSnapshot'],
      ['addCustomService', [{ name: 'value', wire: 'value', source: 'json', codec: jsonCodec('CustomService') }], 'WhaleReinConnectionSnapshot'],
      ['deleteProfile', [{ name: 'id', wire: 'id', source: 'json', codec: jsonCodec('ProfileId') }], 'WhaleReinConnectionSnapshot'],
    ].map(([method, parameters, typeSymbol]) => ({
      id: `whalerein-dsh#whalerein/${method}`,
      service: 'whalereinSettings',
      namespace: 'whalerein',
      method,
      invocation: { kind: 'direct' },
      parameters,
      result: { mode: 'strict', typeSymbol: `whalerein-dsh#${typeSymbol}`, create: () => ({ parse: (value) => value }) },
    }))

    const REMOTE_CONTRIBUTION = {
      package: 'whalerein-dsh',
      descriptors: [{
        id: 'whalerein-dsh#whalerein/snapshot',
        service: 'whalereinSettings',
        namespace: 'whalerein',
        method: 'snapshot',
        invocation: { kind: 'direct' },
        parameters: [],
        result: {
          mode: 'strict',
          typeSymbol: 'whalerein-dsh#WhaleReinSettingsSnapshot',
          create: () => ({ parse: (value) => value }),
        },
      }, {
        id: 'whalerein-dsh#whalerein/invite',
        service: 'whalereinSettings',
        namespace: 'whalerein',
        method: 'invite',
        invocation: { kind: 'direct' },
        parameters: [],
        result: {
          mode: 'strict',
          typeSymbol: 'whalerein-dsh#WhaleReinPairingInvitation',
          create: () => ({ parse: (value) => value }),
        },
      }, {
        id: 'whalerein-dsh#whalerein/revoke',
        service: 'whalereinSettings',
        namespace: 'whalerein',
        method: 'revoke',
        invocation: { kind: 'direct' },
        parameters: [{
          name: 'deviceId',
          wire: 'deviceId',
          source: 'json',
          codec: {
            mode: 'strict',
            typeSymbol: 'whalerein-dsh#DeviceId',
            create: () => ({ parse: (value) => value }),
          },
        }],
        result: {
          mode: 'strict',
          typeSymbol: 'whalerein-dsh#WhaleReinRevokeResult',
          create: () => ({ parse: (value) => value }),
        },
      }, {
        id: 'whalerein-dsh#whalerein/testConnection', service: 'whalereinSettings', namespace: 'whalerein',
        method: 'testConnection', invocation: { kind: 'direct' },
        parameters: [{ name: 'draft', wire: 'draft', source: 'json', codec: {
          mode: 'strict', typeSymbol: 'whalerein-dsh#ConnectionTestDraft', create: () => ({ parse: (value) => value }),
        } }],
        result: { mode: 'strict', typeSymbol: 'whalerein-dsh#ConnectionTestResult', create: () => ({ parse: (value) => value }) },
      }, ...MANAGER_REMOTE_DESCRIPTORS],
    }

    return {
      inject: ['slots', 'locale', 'configForms', 'remote'],
      async apply(ctx) {
        const t = ctx.locale.bind(NS)
        ctx.effect(() => ctx.locale.register(NS, { en, zh }), 'whalerein-dsh: settings dictionaries')
        await ctx.remote.$mount(REMOTE_CONTRIBUTION)
        const form = ctx.configForms.get(ENTRY)
        const developerTools = ctx.configForms.developerTools.enabled
        // Namespace services exist only after $mount, and Cordis reveals them
        // to fibers that inject them. Reading them from this plugin throws.
        await ctx.inject(['remote.whalerein', 'remote.settings'], (injected) => {
          const status = createStatusStore(injected.remote)
          const secrets = createSecretPresence(injected.remote)
          const invite = () => injected.remote.whalerein.invite()
          const testConnection = (draft) => injected.remote.whalerein.testConnection(draft)
          const revoke = (deviceId) => injected.remote.whalerein.revoke(deviceId)
          injected.effect(() => () => status.dispose(), 'whalerein-dsh: status polling')
          injected.slots.inject('settings.section', () =>
            injected.slots.register(
              {
                name: 'settings.section',
                id: 'whalerein',
                order: 35,
                label: () => t('nav'),
                locale: NS,
                inject: () => ({
                  form, status, developerTools, locale: injected.locale, invite, revoke, secrets, testConnection,
                  connection: createConnectionActions(injected.remote),
                }),
              },
              SettingsPage,
            ),
          )
          injected.slots.inject('plugins.row.config', () =>
            injected.slots.register(
              { name: 'plugins.row.config', key: 'whalerein-dsh#whalerein-dsh', locale: NS },
              ({ view }) => view === 'summary'
                ? h('span', null, t('intro'))
                : h(SettingsPage, {
                  t, form, status, developerTools, locale: injected.locale, invite, revoke, secrets, testConnection,
                  connection: createConnectionActions(injected.remote),
                }),
            ),
          )
        })
      },
    }
  },
})
