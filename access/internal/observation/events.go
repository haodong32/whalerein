package observation

import (
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
)

// Route is the admitted body representation for a query or object operation.
type Route string

const (
	RouteInline    Route = "inline"
	RouteReference Route = "reference"
)

// Outcome is the bounded disposition vocabulary shared by Access events.
type Outcome string

const (
	OutcomeOK      Outcome = "ok"
	OutcomeError   Outcome = "error"
	OutcomeExpired Outcome = "expired"
	OutcomeLate    Outcome = "late"
	OutcomeTimeout Outcome = "timeout"
	// OutcomeFailed and OutcomeReplaced are valid only for access_notice.
	OutcomeFailed   Outcome = "failed"
	OutcomeReplaced Outcome = "replaced"
)

// QueryStage is one Access query-lane boundary.
type QueryStage string

const (
	QueryAdmit   QueryStage = "admit"
	QueryForward QueryStage = "forward"
	QueryReply   QueryStage = "reply"
	QueryExpire  QueryStage = "expire"
)

// CommandStage is one durable command boundary.
type CommandStage string

const (
	CommandAdmit    CommandStage = "admit"
	CommandReceipt  CommandStage = "receipt"
	CommandRecover  CommandStage = "recover"
	CommandTerminal CommandStage = "terminal"
)

// RelayStage is one ciphertext object boundary. The contract names reservation
// "stage"; upload is the physical transfer, finish and verify separate the
// declared length from the digest-verified logical result.
type RelayStage string

const (
	RelayReserve RelayStage = "stage"
	RelayUpload  RelayStage = "upload"
	RelayFinish  RelayStage = "finish"
	RelayFetch   RelayStage = "fetch"
	RelayVerify  RelayStage = "verify"
)

// NoticeStage is one broker connection/subscription boundary.
type NoticeStage string

const (
	NoticeSubscribe  NoticeStage = "subscribe"
	NoticeReady      NoticeStage = "ready"
	NoticeDisconnect NoticeStage = "disconnect"
)

// ConnectionRole distinguishes the Host connection from a Device/App-session
// client connection. Both connection notices and transport facts name the same
// broker-issued side.
type ConnectionRole string

const (
	RoleHost   ConnectionRole = "host"
	RoleClient ConnectionRole = "client"
)

// TransportDirection is the one broker-path leg a forwarding fact covers.
type TransportDirection string

const (
	// DirectionHostToBroker is the Host's PUBLISH leg into the broker. Access
	// does not currently stamp it: it only observes the broker's subscriber
	// callback, which has no accepted/dropped publish outcome to testify to.
	DirectionHostToBroker TransportDirection = "host_to_broker"
	// DirectionBrokerToClient is Access's republish leg toward the admitted
	// client connection.
	DirectionBrokerToClient TransportDirection = "broker_to_client"
)

// TransportOutcome is the bounded forwarding disposition. It is separate from
// Outcome because "dropped" is only meaningful for access_transport.
type TransportOutcome string

const (
	TransportOutcomeOK      TransportOutcome = "ok"
	TransportOutcomeFailed  TransportOutcome = "failed"
	TransportOutcomeDropped TransportOutcome = "dropped"
)

// QueryEvent carries the required identity and bounded fields of one
// access_query record. Request is a raw business identity; the sink replaces
// it with a run-local token before any record is written.
type QueryEvent struct {
	// Scope is the authenticated host/device scope of the operation. It is
	// exported only as its own opaque token in the "scope" field; the raw
	// scope never leaves the process. The request token stays scope-free so
	// cross-producer request joins keep working.
	Scope   string
	Request string
	Attempt uint64
	Route   Route
	Stage   QueryStage
	// Outcome is omitted when empty.
	Outcome  Outcome
	Bytes    uint64
	HasBytes bool
}

// CommandEvent carries one access_command record. Durable reports the HTTPS
// path; Attempt is the transport attempt for retries and recovery.
type CommandEvent struct {
	Scope    string
	Request  string
	Attempt  uint64
	Stage    CommandStage
	Durable  bool
	Outcome  Outcome
	Bytes    uint64
	HasBytes bool
}

// RelayEvent carries one access_relay record. Request may be empty for a
// pre-admission object; callers pass the object's own message identity in that
// case so the record still has a stable parent token.
type RelayEvent struct {
	Scope    string
	Request  string
	ObjectOp string
	Stage    RelayStage
	Route    Route
	Bytes    uint64
	HasBytes bool
	Outcome  Outcome
}

// NoticeEvent carries one access_notice record for a broker connection
// generation.
type NoticeEvent struct {
	Connection string
	Generation uint64
	Role       ConnectionRole
	Stage      NoticeStage
	Outcome    Outcome
}

// TransportEvent carries one access_transport record for a broker forwarding
// boundary. Connection is a raw connection reference that the sink tokenizes;
// no payload, topic, message id or peer address is part of the event.
//
// The contract deliberately gives this event no stream, sequence or watch
// identity. A transport record therefore attests MQTT transit for one
// connection generation only; it cannot be joined to a specific stream, and a
// consumer must report that per-stream ambiguity instead of deriving a stream
// duration from it.
type TransportEvent struct {
	Connection string
	Generation uint64
	Role       ConnectionRole
	Direction  TransportDirection
	Bytes      uint64
	Outcome    TransportOutcome
}

func (s *Sink) Query(event QueryEvent) {
	if s == nil {
		return
	}
	data := map[string]any{
		"request": s.token.Token(DomainRequest, event.Request),
		"attempt": event.Attempt,
		"route":   string(event.Route),
		"stage":   string(event.Stage),
	}
	if event.Scope != "" {
		data["scope"] = s.token.Token(DomainSession, event.Scope)
	}
	if event.Outcome != "" {
		data["outcome"] = string(event.Outcome)
	}
	if event.HasBytes {
		data["bytes"] = event.Bytes
	}
	s.emit("access_query", data)
}

func (s *Sink) Command(event CommandEvent) {
	if s == nil {
		return
	}
	data := map[string]any{
		"request": s.token.Token(DomainRequest, event.Request),
		"attempt": event.Attempt,
		"stage":   string(event.Stage),
		"durable": event.Durable,
	}
	if event.Scope != "" {
		data["scope"] = s.token.Token(DomainSession, event.Scope)
	}
	if event.Outcome != "" {
		data["outcome"] = string(event.Outcome)
	}
	if event.HasBytes {
		data["bytes"] = event.Bytes
	}
	s.emit("access_command", data)
}

func (s *Sink) Relay(event RelayEvent) {
	if s == nil {
		return
	}
	data := map[string]any{
		"request":   s.token.Token(DomainRequest, event.Request),
		"object_op": s.token.Token(DomainObject, event.ObjectOp),
		"stage":     string(event.Stage),
		"route":     string(event.Route),
	}
	if event.Scope != "" {
		data["scope"] = s.token.Token(DomainSession, event.Scope)
	}
	if event.HasBytes {
		data["bytes"] = event.Bytes
	}
	if event.Outcome != "" {
		data["outcome"] = string(event.Outcome)
	}
	s.emit("access_relay", data)
}

func (s *Sink) Notice(event NoticeEvent) {
	if s == nil {
		return
	}
	data := map[string]any{
		"connection": s.token.Token(DomainConnection, event.Connection),
		"generation": event.Generation,
		"role":       string(event.Role),
		"stage":      string(event.Stage),
	}
	if event.Outcome != "" {
		data["outcome"] = string(event.Outcome)
	}
	s.emit("access_notice", data)
}

// Transport records one broker forwarding fact. Only the declared bounded
// fields are exported; the connection reference is tokenized in its own
// domain.
func (s *Sink) Transport(event TransportEvent) {
	if s == nil {
		return
	}
	data := map[string]any{
		"connection": s.token.Token(DomainConnection, event.Connection),
		"generation": event.Generation,
		"role":       string(event.Role),
		"direction":  string(event.Direction),
		"bytes":      event.Bytes,
		"outcome":    string(event.Outcome),
	}
	s.emit("access_transport", data)
}

// newLifetime returns an opaque run-local process lifetime token. Raw PIDs are
// never exported.
func newLifetime() (string, error) {
	var material [8]byte
	if _, err := rand.Read(material[:]); err != nil {
		return "", fmt.Errorf("observation: generate lifetime: %w", err)
	}
	return "lt-" + hex.EncodeToString(material[:]), nil
}

// Scope binds a client-supplied request id to the authenticated host/device
// pair so equal ids on two devices stay distinct.
func Scope(hostID, deviceID string) string {
	return hostID + "\x00" + deviceID
}

// OutcomeForError maps one operation error to the observation vocabulary.
// expiredErr is the package-specific terminal expiry sentinel.
func OutcomeForError(err, expiredErr error) Outcome {
	switch {
	case err == nil:
		return OutcomeOK
	case errors.Is(err, expiredErr):
		return OutcomeExpired
	default:
		return OutcomeError
	}
}
