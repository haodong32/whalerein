package clientaddr

import (
	"io"
	"net/netip"
	"regexp"
	"time"
)

var (
	// The patterns are deliberately broad; each candidate is accepted only
	// when netip parses it as an address. This catches addresses inside
	// messages and lists without matching timestamps or arbitrary numbers.
	ipv6Pattern = regexp.MustCompile(`(?:[0-9a-fA-F]{0,4}:){2,7}[0-9a-fA-F]{0,4}(?:%[0-9a-zA-Z._-]+)?`)
	ipv4Pattern = regexp.MustCompile(`(?:[0-9]{1,3}\.){3}[0-9]{1,3}`)
)

// Redactor replaces every IP literal written through it with a pseudonym. It
// wraps the log sink so a future handler that formats RemoteAddr (or a library
// error that embeds it) cannot leak a plaintext address.
type Redactor struct {
	writer    io.Writer
	reference *Referencer
}

// NewRedactor wraps a log writer. A nil reference still redacts, using the
// placeholder [client] instead of a pseudonym.
func NewRedactor(writer io.Writer, reference *Referencer) *Redactor {
	return &Redactor{writer: writer, reference: reference}
}

// Write implements io.Writer. It returns len(p) when the transformed record was
// written successfully, which is the contract for a filtering writer.
func (r *Redactor) Write(p []byte) (int, error) {
	now := time.Now().UTC()
	redacted := r.redact(string(p), now)
	if _, err := io.WriteString(r.writer, redacted); err != nil {
		return 0, err
	}
	return len(p), nil
}

func (r *Redactor) redact(text string, now time.Time) string {
	text = ipv6Pattern.ReplaceAllStringFunc(text, func(candidate string) string {
		return r.replace(candidate, now)
	})
	return ipv4Pattern.ReplaceAllStringFunc(text, func(candidate string) string {
		return r.replace(candidate, now)
	})
}

func (r *Redactor) replace(candidate string, now time.Time) string {
	addr, err := netip.ParseAddr(candidate)
	if err != nil {
		return candidate
	}
	addr = addr.WithZone("").Unmap()
	if !addr.IsValid() || addr.IsUnspecified() || addr.IsMulticast() {
		return candidate
	}
	if r.reference == nil {
		return "[client]"
	}
	ref, _ := r.reference.Reference(addr, now)
	if ref == "" {
		return "[client]"
	}
	return ref
}
