//go:build !unix

package payload

import "errors"

// errHeadroomUnsupported keeps the free-space reserve mandatory on unsupported
// platforms instead of silently admitting writes without a bound.
var errHeadroomUnsupported = errors.New("filesystem free-space reserve is unsupported on this platform")

func freeBytes(string) (int64, error) { return 0, errHeadroomUnsupported }
