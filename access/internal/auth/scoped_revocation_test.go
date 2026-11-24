package auth

import (
	"context"
	"errors"
	"testing"

	"github.com/whalerein/whalerein/access/internal/store"
)

func frozenGrant(hostID, deviceID string, enrollment, grant, revocation, deviceRevocation uint64) store.ScopeGrant {
	return store.ScopeGrant{
		HostID: hostID, DeviceID: deviceID,
		EnrollmentGeneration:       enrollment,
		GrantGeneration:            grant,
		RevocationGeneration:       revocation,
		DeviceRevocationGeneration: deviceRevocation,
	}
}

func consoleScope(hostID, deviceID string, enrollment, grant uint64) store.RevocationScope {
	return store.CloudDeviceRevocation{
		HostID: hostID, DeviceID: deviceID,
		EnrollmentGeneration: int64(enrollment),
		GrantGeneration:      grant,
		RevocationGeneration: 1,
		Exists:               true,
	}.Scope()
}

func TestConsoleGrantRevocationFreezesScopeAndRecordsEnforcedConnection(t *testing.T) {
	service, fs, admission, presence, transport, enforcer := newServiceWithFakes(t)
	ctx := context.Background()
	fs.grants[grantKey("h1", "d1")] = true
	fs.grantGens[grantKey("h1", "d1")] = store.AdmissionGrant{
		HostID: "h1", DeviceID: "d1", Active: true,
		GrantGeneration: 2, RevocationGeneration: 3, DeviceRevocationGeneration: 4,
		EnrollmentGeneration: 5,
	}
	fs.enrollments["h1"] = store.ExternalEnrollment{HostID: "h1", Generation: 5}
	connection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 7}, ClientID: "whalerein-device-d1"}
	presence.connections = map[string][]store.ScopeConnection{"d1": {connection}}
	enforcer.applied = []store.ScopeConnection{connection}

	receipt, err := service.EnforceRevocationScope(ctx, consoleScope("h1", "d1", 5, 2))
	if err != nil {
		t.Fatal(err)
	}
	if !admission.denied[grantKey("h1", "d1")] {
		t.Fatal("the revoked grant was not fenced in local admission")
	}
	if fs.grants[grantKey("h1", "d1")] {
		t.Fatal("the frozen grant generation was not revoked")
	}
	if len(enforcer.calls) != 1 || enforcer.calls[0] != "grant:h1:d1" {
		t.Fatalf("enforcer calls = %v", enforcer.calls)
	}
	if len(enforcer.scope.Grants) != 1 || len(enforcer.scope.Grants[0].Connections) != 1 ||
		enforcer.scope.Grants[0].Connections[0] != connection {
		t.Fatalf("frozen connection scope = %+v", enforcer.scope)
	}
	// The console receipt advanced the revocation generation for the fence.
	if enforcer.scope.Grants[0].Revision() != (2<<20 | 2) {
		t.Fatalf("fence revision = %d", enforcer.scope.Grants[0].Revision())
	}
	if receipt.Primary() == nil || *receipt.Primary() != connection.Ref {
		t.Fatalf("applied connection evidence = %+v", receipt.Primary())
	}
	// Notification is the Host channel, never an unfenced username disconnect.
	if len(transport.disconnected) != 0 {
		t.Fatalf("console revocation used a username disconnect: %v", transport.disconnected)
	}
}

func TestLastGrantDeviceRevocationTargetsOnlyTheFrozenDeviceConnection(t *testing.T) {
	service, fs, _, presence, transport, enforcer := newServiceWithFakes(t)
	ctx := context.Background()
	fs.devices["d1"] = store.Device{DeviceID: "d1"}
	fs.hosts["h1"] = store.Host{HostID: "h1"}
	fs.grants[grantKey("h1", "d1")] = true
	fs.grantGens[grantKey("h1", "d1")] = store.AdmissionGrant{
		HostID: "h1", DeviceID: "d1", Active: true,
		GrantGeneration: 4, RevocationGeneration: 1, DeviceRevocationGeneration: 2,
	}
	connection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 9}, ClientID: "whalerein-device-d1"}
	presence.connections = map[string][]store.ScopeConnection{"d1": {connection}}

	if err := service.RevokeDevice(ctx, "d1"); err != nil {
		t.Fatal(err)
	}
	if len(enforcer.calls) != 1 || enforcer.calls[0] != "device:d1" {
		t.Fatalf("enforcer calls = %v", enforcer.calls)
	}
	if len(enforcer.scope.Grants) != 1 || enforcer.scope.Grants[0].DeviceID != "d1" {
		t.Fatalf("device scope = %+v", enforcer.scope)
	}
	if len(enforcer.scope.Grants[0].Connections) != 1 || enforcer.scope.Grants[0].Connections[0] != connection {
		t.Fatalf("frozen device connection = %+v", enforcer.scope.Grants[0].Connections)
	}
	if len(transport.disconnected) != 0 {
		t.Fatalf("device revocation used a username disconnect: %v", transport.disconnected)
	}
}

func TestFullDeviceRevocationFencesEveryFrozenHostScope(t *testing.T) {
	service, fs, admission, presence, _, enforcer := newServiceWithFakes(t)
	ctx := context.Background()
	fs.devices["d1"] = store.Device{DeviceID: "d1"}
	fs.grants[grantKey("h1", "d1")] = true
	fs.grants[grantKey("h2", "d1")] = true
	fs.grantGens[grantKey("h1", "d1")] = store.AdmissionGrant{HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 5}
	fs.grantGens[grantKey("h2", "d1")] = store.AdmissionGrant{HostID: "h2", DeviceID: "d1", Active: true, GrantGeneration: 6}
	connection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 3}, ClientID: "whalerein-device-d1"}
	presence.connections = map[string][]store.ScopeConnection{"d1": {connection}}

	if err := service.RevokeDevice(ctx, "d1"); err != nil {
		t.Fatal(err)
	}
	if len(enforcer.scope.Grants) != 2 {
		t.Fatalf("frozen grants = %+v", enforcer.scope.Grants)
	}
	for _, grant := range enforcer.scope.Grants {
		if len(grant.Connections) != 1 || grant.Connections[0] != connection {
			t.Fatalf("grant %s lost the frozen connection: %+v", grant.HostID, grant)
		}
	}
	if !admission.denied[grantKey("h1", "d1")] || !admission.denied[grantKey("h2", "d1")] {
		t.Fatalf("admission fences = %+v", admission.denied)
	}
}

func TestManagedHostDisableAfterReAddDoesNotFenceOrCloseTheNewConnection(t *testing.T) {
	service, fs, admission, presence, _, enforcer := newServiceWithFakes(t)
	ctx := context.Background()
	// The Host was re-added under the same stable id: the current enrollment
	// generation is newer than the frozen removal.
	fs.enrollments["h1"] = store.ExternalEnrollment{HostID: "h1", Generation: 2}
	newHostConnection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-b", ID: 12}, ClientID: "whalerein-host-h1"}
	presence.hostConns = map[string]store.ScopeConnection{"h1": newHostConnection}
	presence.connections = map[string][]store.ScopeConnection{
		"d1": {{Ref: store.ConnectionRef{Epoch: "epoch-b", ID: 13}, ClientID: "whalerein-device-d1"}},
	}
	scope := store.RevocationScope{
		HostID:               "h1",
		EnrollmentGeneration: 1,
		Grants:               []store.ScopeGrant{frozenGrant("h1", "d1", 1, 4, 2, 1)},
	}
	if _, err := service.DisableHostScope(ctx, scope); err != nil {
		t.Fatal(err)
	}
	if len(admission.denied) != 0 {
		t.Fatalf("late cleanup fenced the re-added Host: %+v", admission.denied)
	}
	if enforcer.scope.HostConnection != nil {
		t.Fatalf("late cleanup closed the re-added Host connection: %+v", enforcer.scope.HostConnection)
	}
	for _, grant := range enforcer.scope.Grants {
		if len(grant.Connections) != 0 {
			t.Fatalf("late cleanup froze a new device connection: %+v", grant.Connections)
		}
	}
	if len(enforcer.calls) == 0 || enforcer.calls[0] != "host:h1" {
		t.Fatalf("the old generation was not fenced: %v", enforcer.calls)
	}
}

func TestScopedGrantRetryLeavesANewerGenerationUntouched(t *testing.T) {
	service, fs, admission, _, _, enforcer := newServiceWithFakes(t)
	ctx := context.Background()
	// A re-pair replaced the frozen grant generation before the retry.
	fs.grants[grantKey("h1", "d1")] = true
	fs.grantGens[grantKey("h1", "d1")] = store.AdmissionGrant{
		HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 9,
	}
	fs.enrollments["h1"] = store.ExternalEnrollment{HostID: "h1", Generation: 1}
	scope := consoleScope("h1", "d1", 1, 2)
	scope.Grants[0].RevocationGeneration = 1
	if _, err := service.EnforceRevocationScope(ctx, scope); err != nil {
		t.Fatal(err)
	}
	if !fs.grants[grantKey("h1", "d1")] {
		t.Fatal("the retry revoked a newer grant generation")
	}
	if admission.denied[grantKey("h1", "d1")] {
		t.Fatal("the retry fenced a newer grant generation in admission")
	}
	if len(enforcer.calls) != 1 {
		t.Fatalf("enforcer calls = %v", enforcer.calls)
	}
	if enforcer.scope.Grants[0].GrantGeneration != 2 {
		t.Fatalf("retry lost the frozen generation: %+v", enforcer.scope.Grants[0])
	}
}

func TestDeviceRevocationPendingWhenBrokerIsUnavailableThenRetries(t *testing.T) {
	service, fs, _, presence, _, enforcer := newServiceWithFakes(t)
	ctx := context.Background()
	fs.devices["d1"] = store.Device{DeviceID: "d1"}
	fs.grants[grantKey("h1", "d1")] = true
	connection := store.ScopeConnection{Ref: store.ConnectionRef{Epoch: "epoch-a", ID: 3}, ClientID: "whalerein-device-d1"}
	presence.connections = map[string][]store.ScopeConnection{"d1": {connection}}
	enforcer.err = errors.New("broker unavailable")
	if err := service.RevokeDevice(ctx, "d1"); err == nil {
		t.Fatal("a Broker failure was hidden")
	}
	if fs.grants[grantKey("h1", "d1")] {
		t.Fatal("the durable revocation was rolled back")
	}
	enforcer.err = nil
	if err := service.RevokeDevice(ctx, "d1"); err != nil {
		t.Fatal(err)
	}
	if len(enforcer.calls) != 2 || enforcer.calls[1] != "device:d1" {
		t.Fatalf("retry did not re-issue the scoped enforcement: %v", enforcer.calls)
	}
}

func TestScopedRevocationSurfacesStoreFailuresAndRefusesEmptyScopes(t *testing.T) {
	service, fs, _, _, _, _ := newServiceWithFakes(t)
	ctx := context.Background()
	if _, err := service.EnforceRevocationScope(ctx, store.RevocationScope{}); !errors.Is(err, ErrBadRequest) {
		t.Fatalf("empty scope error = %v", err)
	}
	fs.enrollmentErr = errors.New("enrollment store down")
	if _, err := service.EnforceRevocationScope(ctx, consoleScope("h1", "d1", 1, 1)); err == nil {
		t.Fatal("an enrollment read failure must be surfaced")
	}
	fs.enrollmentErr = nil
	fs.grants[grantKey("h1", "d1")] = true
	fs.revokeGenerationErr = errors.New("grant store down")
	if _, err := service.EnforceRevocationScope(ctx, consoleScope("h1", "d1", 1, 1)); err == nil {
		t.Fatal("a durable revoke failure must be surfaced")
	}
	fs.revokeGenerationErr = nil
	fs.listGrantErr = errors.New("grant list down")
	if err := service.RevokeDevice(ctx, "d1"); err == nil {
		t.Fatal("a device grant list failure must be surfaced")
	}
}

func TestDeviceRevocationSkipsAGrantFromASupersededEnrollment(t *testing.T) {
	service, fs, admission, _, _, enforcer := newServiceWithFakes(t)
	ctx := context.Background()
	fs.devices["d1"] = store.Device{DeviceID: "d1"}
	fs.grants[grantKey("h1", "d1")] = true
	fs.grantGens[grantKey("h1", "d1")] = store.AdmissionGrant{
		HostID: "h1", DeviceID: "d1", Active: true, GrantGeneration: 2, EnrollmentGeneration: 1,
	}
	fs.enrollments["h1"] = store.ExternalEnrollment{HostID: "h1", Generation: 2}
	if err := service.RevokeDevice(ctx, "d1"); err != nil {
		t.Fatal(err)
	}
	if admission.denied[grantKey("h1", "d1")] {
		t.Fatal("a superseded enrollment was fenced in local admission")
	}
	if len(enforcer.calls) != 1 || len(enforcer.scope.Grants) != 0 {
		t.Fatalf("superseded grant entered the device scope: %+v", enforcer.scope)
	}
}
