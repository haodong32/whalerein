// Export native errors for sanitization by the collector; omit captured stdout/stderr.
export default async function* report(source) {
  for await (const event of source) {
    if (['test:pass', 'test:fail'].includes(event.type)) {
      const d = event.data;
      const error = d.details?.error?.cause ?? d.details?.error;
      const diagnostic = error ? [error.name, error.message, error.stack].filter(Boolean).join('\n') : undefined;
      yield JSON.stringify({duration_ms:d.details?.duration_ms, error:diagnostic, skip_reason:typeof d.skip === 'string' ? d.skip : undefined, name:d.name, file:d.file, line:d.line,
        nesting:d.nesting, type:d.details?.type,
        status:d.details?.error?.failureType === 'cancelledByParent' ? 'cancelled' : d.skip ? 'skipped' : d.todo ? 'todo' :
          event.type === 'test:pass' ? 'passed' : 'failed'}) + '\n';
    }
    if (event.type === 'test:coverage') {
      yield JSON.stringify({coverage: event.data.summary.totals}) + '\n';
    }
    if (event.type === 'test:summary') {
      yield JSON.stringify({summary:true, file:event.data.file,
        counts:event.data.counts}) + '\n';
    }
  }
}
