//go:build unix

package payload

import "syscall"

// freeBytes reports the unprivileged free space available at path. The
// configured reserve is compared with this value before admission.
func freeBytes(path string) (int64, error) {
	var stat syscall.Statfs_t
	if err := syscall.Statfs(path, &stat); err != nil {
		return 0, err
	}
	return int64(stat.Bavail) * int64(stat.Bsize), nil
}
