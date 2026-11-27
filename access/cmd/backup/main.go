// Command whalerein-access-backup creates, verifies and restores the offline
// backup of a Standalone Access database. The installation key is the original
// installation credential, never a key minted after disk loss.
package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"time"

	"github.com/whalerein/whalerein/access/internal/standalone"
	"github.com/whalerein/whalerein/access/internal/store"
)

func main() { os.Exit(run(os.Args[1:], os.Stdout, os.Stderr)) }

func run(args []string, stdout, stderr io.Writer) int {
	if len(args) == 0 {
		fmt.Fprintln(stderr, "usage: whalerein-access-backup <create|verify|restore> [flags]")
		return 2
	}
	var err error
	switch args[0] {
	case "create":
		err = create(args[1:], stdout)
	case "verify":
		err = verify(args[1:], stdout)
	case "restore":
		err = restore(args[1:], stdout)
	default:
		fmt.Fprintf(stderr, "unknown command %q\n", args[0])
		return 2
	}
	if err != nil {
		fmt.Fprintf(stderr, "backup: %v\n", err)
		return 1
	}
	return 0
}

func create(args []string, stdout io.Writer) error {
	flags := flag.NewFlagSet("create", flag.ContinueOnError)
	database := flags.String("database", "", "source SQLite database")
	destination := flags.String("out", "", "backup database path")
	installation := flags.String("installation-id", "", "installation id")
	keyFile := flags.String("key-file", "", "base64 installation key file")
	revision := flags.Uint64("revision", 0, "final committed authorization revision")
	if err := flags.Parse(args); err != nil {
		return err
	}
	key, err := readKey(*keyFile)
	if err != nil {
		return err
	}
	data, err := store.Open(context.Background(), *database)
	if err != nil {
		return err
	}
	defer data.Close()
	manifest, err := standalone.Create(context.Background(), data, *destination, *installation, key, *revision, time.Now())
	if err != nil {
		return err
	}
	return json.NewEncoder(stdout).Encode(manifest)
}

func verify(args []string, stdout io.Writer) error {
	flags := flag.NewFlagSet("verify", flag.ContinueOnError)
	database := flags.String("database", "", "backup database path")
	manifest := flags.String("manifest", "", "manifest path")
	installation := flags.String("installation-id", "", "installation id")
	keyFile := flags.String("key-file", "", "base64 installation key file")
	floor := flags.Uint64("floor", 0, "proven authorization revision floor")
	if err := flags.Parse(args); err != nil {
		return err
	}
	key, err := readKey(*keyFile)
	if err != nil {
		return err
	}
	verified, err := standalone.Verify(*database, *manifest, key, *installation, *floor)
	if err != nil {
		return err
	}
	return json.NewEncoder(stdout).Encode(verified)
}

func restore(args []string, stdout io.Writer) error {
	flags := flag.NewFlagSet("restore", flag.ContinueOnError)
	backup := flags.String("backup", "", "backup database path")
	manifest := flags.String("manifest", "", "manifest path")
	target := flags.String("target", "", "new database path (must not exist)")
	installation := flags.String("installation-id", "", "installation id")
	keyFile := flags.String("key-file", "", "base64 installation key file")
	floor := flags.Uint64("floor", 0, "proven authorization revision floor")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if *target == "" {
		return errors.New("target is required")
	}
	key, err := readKey(*keyFile)
	if err != nil {
		return err
	}
	verified, err := standalone.Restore(context.Background(), *backup, *manifest, *target, key, *installation, *floor)
	if err != nil {
		return err
	}
	return json.NewEncoder(stdout).Encode(verified)
}

func readKey(path string) ([]byte, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read installation key: %w", err)
	}
	for _, decoder := range []*base64.Encoding{base64.StdEncoding, base64.RawStdEncoding, base64.RawURLEncoding} {
		if decoded, err := decoder.DecodeString(string(trimSpace(raw))); err == nil && len(decoded) >= 32 {
			return decoded, nil
		}
	}
	if len(raw) >= 32 {
		return raw, nil
	}
	return nil, errors.New("installation key must be at least 32 bytes or base64 for them")
}

func trimSpace(raw []byte) string {
	start, end := 0, len(raw)
	for start < end && (raw[start] == ' ' || raw[start] == '\n' || raw[start] == '\r' || raw[start] == '\t') {
		start++
	}
	for end > start && (raw[end-1] == ' ' || raw[end-1] == '\n' || raw[end-1] == '\r' || raw[end-1] == '\t') {
		end--
	}
	return string(raw[start:end])
}
