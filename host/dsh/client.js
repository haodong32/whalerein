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
      fieldRow: { display: 'grid', gridTemplateColumns: 'minmax(0, 1fr) minmax(180px, 240px)', alignItems: 'center', gap: '24px', padding: '16px 0', borderBottom: '0.5px solid var(--dsw-alias-border-l2)' },
      fieldCopy: { minWidth: 0, display: 'flex', flexDirection: 'column', gap: '4px' },
      fieldTitle: { display: 'flex', alignItems: 'center', flexWrap: 'wrap', gap: '8px', fontSize: '14px', fontWeight: 400, lineHeight: '20px' },
      fieldSubtitle: { margin: 0, color: 'var(--dsw-alias-label-secondary)', fontSize: '12px', lineHeight: '18px' },
      fieldControl: { minWidth: 0, display: 'flex', justifyContent: 'flex-end' },
      formActions: { display: 'flex', alignItems: 'center', flexWrap: 'wrap', gap: '8px', paddingTop: '16px' },
      details: { borderTop: '1px solid var(--dsw-alias-border-l2)', paddingTop: '16px' },
      form: { display: 'flex', flexDirection: 'column', gap: '16px', paddingTop: '12px' },
      diagnosticRow: { display: 'grid', gridTemplateColumns: 'minmax(140px, 0.7fr) minmax(0, 1.3fr)', gap: '16px', padding: '8px 0', fontSize: '13px' },
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
      @media (max-width: 560px) {
        .whalerein-settings .whalerein-field-row { grid-template-columns: minmax(0, 1fr); gap: 12px; }
        .whalerein-settings .whalerein-field-control { justify-content: flex-start; }
        .whalerein-settings .whalerein-diagnostic-row { grid-template-columns: minmax(0, 1fr); gap: 6px; }
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

    function Field({ field, draft, t, onChange, secretSet, disabled, id, menuOpen, setMenuOpen }) {
      const label = t(field.label)
      const subtitleId = `${id}-subtitle`
      let control
      if (field.kind === 'select') {
        control = h(Menu, {
          open: menuOpen && !disabled,
          anchor: h(Button, {
            variant: 'ghost', size: 'md', className: 'whalerein-select', disabled,
            'aria-labelledby': `${id}-label ${id}-value`,
            'aria-describedby': subtitleId,
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
          className: 'whalerein-input', 'aria-label': label, 'aria-describedby': subtitleId,
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
          h('p', { id: subtitleId, style: styles.fieldSubtitle }, t(field.subtitle))),
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

    function DeviceRow({ device, t, locale, confirming, revoking, error, onAsk, onCancel, onConfirm }) {
      const time = formatTime(device.pairedAt, locale)
      const name = String(device.displayName ?? '').trim()
      const title = name || device.deviceId
      const os = osLabel(device.os)
      const identity = name ? h('span', { style: styles.deviceId }, device.deviceId) : null
      const status = h(Tag, { tone: device.connected ? 'success' : 'quiet' },
        h(StateDot, { state: device.connected ? 'done' : 'idle', size: 10 }),
        device.connected ? t('deviceConnected') : t('devicePaired'))
      const revokeControls = confirming
        ? h('div', { style: styles.deviceActions },
          h(Button, { variant: 'ghost', size: 'sm', disabled: revoking, onClick: onCancel }, t('revokeCancel')),
          h(Button, { variant: 'primary', size: 'sm', style: styles.revokeConfirm, disabled: revoking, onClick: onConfirm }, revoking ? t('revoking') : t('revokeConfirm')))
        : h(Button, { variant: 'ghost', size: 'sm', style: styles.revokeAction, onClick: onAsk }, t('revoke'))
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

    function ConnectionCard({ t, snapshot, inviting, inviteError, pairingUri, copyFeedback, onGenerate, onCopy, onRefresh, titleId }) {
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
              h('p', { style: styles.hint }, t('pairingUriHint'))),
            h(Button, { variant: 'outline', size: 'sm', onClick: onCopy }, t('copyUri'))),
          h('code', { style: styles.code, 'aria-label': t('pairingUri') }, pairingUri),
          copyFeedback ? h('p', { style: copyFeedback === 'uriCopied' ? styles.success : styles.error, role: 'status' }, t(copyFeedback)) : null) : null)
    }

    function SettingsPage({ t, form, status, developerTools, locale, invite, revoke, secrets, testConnection }) {
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
      const [pairingUri, setPairingUri] = React.useState('')
      const [inviteError, setInviteError] = React.useState('')
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
          if (!cancelled) setDraft(initialDraft(snapshot.value))
        })
        return () => {
          cancelled = true
        }
      }, [snapshot.revision])
      React.useEffect(() => {
        setConfigurationOpen(!statusSnapshot.connected)
      }, [statusSnapshot.connected])

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
      const copyUri = async () => {
        setCopyFeedback('')
        try {
          const copied = pairingUri && await writeClipboard(pairingUri)
          setCopyFeedback(copied ? 'uriCopied' : 'copyFailed')
        } catch {
          setCopyFeedback('copyFailed')
        }
      }
      const generateLink = async () => {
        setInviteError('')
        setPairingUri('')
        setCopyFeedback('')
        setInviting(true)
        try {
          const result = await invite()
          const uri = result?.ok ? result.value?.pairingUri : ''
          if (typeof uri === 'string' && uri.startsWith('whalerein://pair?')) {
            setPairingUri(uri)
            return
          }
          setInviteError(inviteErrorKey(result?.error?.code))
        } catch (caught) {
          setInviteError(inviteErrorKey(caught?.code))
        } finally {
          setInviting(false)
        }
      }
      const confirmRevoke = async (deviceId) => {
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
          setRevoking(false)
        }
      }
      const save = async () => {
        if (!snapshot.writable || testing) return
        testVersion.current += 1
        setTestResult(null)
        setSaving(true)
        setError('')
        setSaved(false)
        try {
          for (const field of FIELDS) {
            const key = field.key
            if (SECRET_FIELDS.has(key) && draft[key] === '') continue
            if (key === 'host_id' && String(draft[key] ?? '').trim() === '') continue
            const okay = draft[key] === '' ? await form.unset(key) : await form.set(key, draft[key])
            if (!okay) {
              setError(tr('saveFailed'))
              return
            }
          }
          if (String(draft.server ?? '').trim() !== '') {
            const clearedMqtt = await form.unset('mqtt_url')
            const clearedAccess = await form.unset('access_url')
            if (!clearedMqtt || !clearedAccess) {
              setError(tr('saveFailed'))
              return
            }
          }
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

      return h('div', { style: styles.page, className: 'whalerein-settings' },
        h('style', null, css),
        h('header', { style: styles.header },
          h('h2', { style: styles.title }, tr('title'))),
        h(ConnectionCard, {
          t: tr, snapshot: statusSnapshot, inviting, inviteError, pairingUri, copyFeedback,
          onGenerate: generateLink, onCopy: copyUri, onRefresh: () => status.refresh(),
          titleId: `${fieldId}-connection-title`,
        }),
        h('section', { style: styles.section, 'aria-labelledby': 'whalerein-devices-title' },
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
              error: revokeError.id === device.deviceId ? revokeError.key : '',
              onAsk: () => { setRevokeTarget(device.deviceId); setRevokeError({ id: '', key: '' }) },
              onCancel: () => { if (!revoking) setRevokeTarget('') },
              onConfirm: () => confirmRevoke(device.deviceId),
            })))
            : h('p', { style: styles.hint }, tr('noDevices'))),
        h('section', { style: styles.details },
          h(DisclosureRow, {
            title: tr('configuration'), icon: h(IconSettingsOutlineRegular, { size: 16 }),
            open: configurationOpen, expandable: true, expandOnRowClick: true,
            onToggle: () => { setMenuOpen(false); setConfigurationOpen((open) => !open) },
          }, h('div', { style: styles.form },
            h('p', { style: styles.hint }, tr('configurationHint')),
            !snapshot.writable ? h('p', { style: styles.hint }, tr('readOnly')) : null,
            h('div', null,
              primaryFields.map((field) => h(Field, { key: field.key, field, draft, t: tr, onChange: setField, secretSet: secrets.has(field.key), id: `${fieldId}-${field.key}`, disabled: saving || !snapshot.writable })),
              h(DisclosureRow, {
                title: tr('advanced'), icon: h(IconSlidersTwoOutlineRegular, { size: 16 }),
                open: advancedOpen, expandable: true, expandOnRowClick: true,
                onToggle: () => { setMenuOpen(false); setAdvancedOpen((open) => !open) },
              }, h('div', { style: styles.form },
                advancedFields.map((field) => h(Field, { key: field.key, field, draft, t: tr, onChange: setField, id: `${fieldId}-${field.key}`, menuOpen, setMenuOpen, disabled: saving || !snapshot.writable }))))),
            // The official form has a fixed footer; compose official buttons
            // here so saving and testing share one accessible action row.
            h('div', { style: styles.formActions },
              h(Button, { variant: 'primary', size: 'sm', disabled: saving || testing || !snapshot.writable || !FIELDS.some((field) => draft[field.key] !== baseline[field.key]), onClick: save }, saving ? tr('saving') : tr('save')),
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

    function createStatusStore(remote) {
      let snapshot = { available: false, connected: false, hostId: '', pairingCode: '', devices: [] }
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
                available: result.value.available === true,
                connected: result.value.connected === true,
                hostId: typeof result.value.hostId === 'string' ? result.value.hostId : '',
                pairingCode: typeof result.value.pairingCode === 'string' ? result.value.pairingCode : '',
                devices: Array.isArray(result.value.devices) ? result.value.devices : [],
                diagnostics: result.value.diagnostics ?? null,
              })
            } else {
              publish({ ...snapshot, available: false, connected: false, pairingCode: '', devices: [] })
            }
          } catch {
            if (!disposed) publish({ ...snapshot, available: false, connected: false, pairingCode: '', devices: [] })
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
      }],
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
                inject: () => ({ form, status, developerTools, locale: injected.locale, invite, revoke, secrets, testConnection }),
              },
              SettingsPage,
            ),
          )
          injected.slots.inject('plugins.row.config', () =>
            injected.slots.register(
              { name: 'plugins.row.config', key: 'whalerein-dsh#whalerein-dsh', locale: NS },
              ({ view }) => view === 'summary'
                ? h('span', null, t('intro'))
                : h(SettingsPage, { t, form, status, developerTools, locale: injected.locale, invite, revoke, secrets, testConnection }),
            ),
          )
        })
      },
    }
  },
})
