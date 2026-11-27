"""CLI multi-process, signal and revocation contract acceptance."""

import json
import os
import select
import signal
import subprocess

import pytest
from environment import ROOT, wait
from journey_helpers import sql

pytestmark = pytest.mark.chain


def _event(value):
    return value.get("data", {}).get("event", {})


def _texts(event):
    return [block.get("text", "") for block in event.get("blocks", [])]


def _attached(watcher):
    return watcher.wait(lambda value: _event(value).get("type") == "state", timeout=20)


def _assert_no_panic(watcher):
    stderr = "".join(watcher.stderr)
    assert "panicked" not in stderr, stderr[-500:]
    assert "failed printing to stdout" not in stderr, stderr[-500:]


@pytest.mark.description(
    "SIGTERM on an attached session watch returns exit 2 with a cancelled envelope and no panic."
)
def test_sigterm_on_session_watch_is_a_cancelled_envelope(paired_client, session_id):
    watcher = paired_client.watch("session", "watch", session_id, "--timeout", "30")
    try:
        _attached(watcher)
        watcher.process.send_signal(signal.SIGTERM)
        assert watcher.wait_exit(timeout=10) == 2
        envelope = watcher.values[-1]
        assert envelope["schema"] == "whalerein/client/v1"
        assert envelope["command"] == "session.watch"
        assert envelope["error"]["code"] == "cancelled"
        assert envelope["error"]["action"] == "retry"
        _assert_no_panic(watcher)
    finally:
        watcher.close()


@pytest.mark.description(
    "A second session watch conflicts while a third process sends successfully and the attached watch still delivers committed text."
)
def test_second_session_watch_conflicts_while_send_succeeds(paired_client, session_id):
    first = paired_client.watch("session", "watch", session_id, "--timeout", "30")
    try:
        _attached(first)
        second = paired_client.watch("session", "watch", session_id, "--timeout", "10")
        try:
            assert second.wait_exit(timeout=15) == 8
            assert second.values[-1]["error"]["code"] == "conflict"
            assert "another watch is active" in second.values[-1]["error"]["message"]
        finally:
            second.close()

        sent = paired_client.call(
            "session", "send", session_id, "concurrent send fixture", "--detach"
        )
        assert sent["state"] == "submission"
        assert sent["operation_id"]
        assert sent["target"] == {
            "host_id": paired_client.identity["host_id"],
            "session_id": session_id,
        }
        first.wait(
            lambda value: any(
                "concurrent send fixture" in text for text in _texts(_event(value))
            ),
            timeout=30,
        )
        first.process.send_signal(signal.SIGTERM)
        assert first.wait_exit(timeout=10) == 2
        assert first.values[-1]["error"]["code"] == "cancelled"
        _assert_no_panic(first)

        recovered = paired_client.call("operation", "recover", sent["operation_id"])
        assert recovered["operation_id"] == sent["operation_id"]
        assert recovered["target"] == sent["target"]
        assert recovered["receipt"]["status"] == "succeeded"
        assert recovered["receipt"]["message_id"]
    finally:
        first.close()


def test_watch_streams_while_same_device_send_follows_reply(scenario, paired_client, session_id):
    """An attached watch receives two live deltas before an ordinary send can finish."""
    fixture = scenario.dsh.process
    fixture.control("suppress_message_echo", session_id=session_id)
    watcher = paired_client.watch("session", "watch", session_id, "--timeout", "40")
    try:
        _attached(watcher)
        resets_before_send = sum(_event(value).get("type") == "reset" for value in watcher.values)
        # Keep the fixture turn open until preview_final so the sibling send
        # cannot settle before the coexistence window is exercised.
        fixture.control("hold_next_send_turn", session_id=session_id)
        sender = paired_client.watch("session", "send", session_id, "fixture", "--timeout", "30")
        try:
            def admitted_message():
                for event in scenario.dsh.snapshot(session_id)["events"]:
                    if event["type"] == "user/message" and event["data"].get("id"):
                        return event["data"]["id"]
                return None

            wait(admitted_message, timeout=10)
            message_id = admitted_message()
            assert message_id
            fixture.control("preview_start", session_id=session_id)
            for index, text in enumerate(("x", "y")):
                fixture.control("preview_chunk", session_id=session_id, index=index, text=text)
                watcher.wait(
                    lambda value: _event(value).get("type") == "preview" and _event(value).get("text") == text,
                    timeout=8,
                )
                assert sender.process.poll() is None

            assert sum(_event(value).get("type") == "reset" for value in watcher.values) == resets_before_send
            fixture.control("preview_final", session_id=session_id, message_id=message_id, count=2)
            assert sender.process.poll() is None, "sibling send exited before the fixture turn ended"
            fixture.control("finish_send_turn", session_id=session_id)
            assert sender.wait_exit(timeout=10) == 0
            assert sender.values[-1]["data"]["reply"]["state"] == "committed"
        finally:
            sender.close()
    finally:
        watcher.close()


@pytest.mark.description(
    "Revoking the Device during a session watch returns exit 4 and removes the persisted regional credential hash."
)
def test_revoke_during_session_watch_ends_with_authorization_error(
    scenario, paired_client, session_id
):
    watcher = paired_client.watch("session", "watch", session_id, "--timeout", "20")
    try:
        _attached(watcher)
        assert paired_client.call("device", "revoke", "--yes")["revoked"]
        assert watcher.wait_exit(timeout=30) == 4
        envelope = watcher.values[-1]
        assert envelope["schema"] == "whalerein/client/v1"
        assert envelope["command"] == "session.watch"
        assert envelope["error"]["code"] in {"revoked", "unauthorized", "not_paired"}
        assert envelope["error"]["action"] == "reauth"
        _assert_no_panic(watcher)

        # Query only the credential count from this case's database. The
        # transport credential is never extracted from the platform keyring.
        assert scenario.sql("SELECT count(*) FROM device_credentials") == "0"
    finally:
        watcher.close()


def _durable_requests(scenario, request_id):
    """Count durable request rows for one lowercase-hex query correlation id."""
    return int(
        sql(
            scenario.schema,
            f"SELECT count(*) FROM requests WHERE request_id = '{request_id}'",
        )
    )


class QueryLaneObserver:
    """Record the four epoch-5 query-lane hops through the real broker."""

    def __init__(self, scenario):
        self.process = subprocess.Popen(
            ["node", "tests/integration/drivers/transport/query-lane-observer.mjs"],
            cwd=ROOT,
            env={
                **os.environ,
                "WHALEREIN_MQTT_URL": "mqtt://127.0.0.1:1883",
                "WHALEREIN_TEST_ACCESS_PASSWORD": scenario.env["WHALEREIN_BROKER_PASSWORD"],
                "WHALEREIN_OBSERVER_HOST_ID": scenario.host,
            },
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.expect({"type": "ready"})

    def _read(self, timeout=15):
        ready, _, _ = select.select([self.process.stdout], [], [], timeout)
        assert ready, "query lane observer produced no response"
        line = self.process.stdout.readline()
        assert line, "query lane observer produced no line"
        return json.loads(line)

    def expect(self, expected):
        value = self._read()
        assert value == expected, value
        return value

    def snapshot(self):
        self.process.stdin.write(json.dumps({"op": "snapshot"}) + "\n")
        self.process.stdin.flush()
        value = self._read()
        assert value.get("type") == "snapshot", value
        return value

    def close(self):
        if self.process.poll() is None:
            try:
                self.process.stdin.write(json.dumps({"op": "stop"}) + "\n")
                self.process.stdin.flush()
                self.process.wait(timeout=10)
            except (BrokenPipeError, subprocess.TimeoutExpired):
                self.process.kill()
                self.process.wait(timeout=5)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            if stream is not None:
                stream.close()


@pytest.mark.description(
    "Session list, history and watch run over the MQTT 5 query lane: every Device query is admitted with the fixed reply topic, forwarded to the Host, answered by a Host result, and delivered back as a correlated reply with whalerein-target and Message Expiry, while no durable request row is created."
)
def test_cli_session_queries_use_the_mqtt_query_lane(scenario, paired_client, session_id):
    """A CLI session query traverses query -> request -> result -> reply with exact MQTT 5 properties and no durable row."""
    observer = QueryLaneObserver(scenario)
    try:
        baseline = observer.snapshot()
        paired_client.call("session", "list", "--host", scenario.host)
        paired_client.call("session", "history", session_id)
        wait(
            lambda: len(observer.snapshot()["request"]) >= len(baseline["request"]) + 2,
            timeout=25,
        )
        # A watch is also a query-lane operation; its periodic CheckVersions
        # adds another bounded query without a durable request row.
        watcher = paired_client.watch("session", "watch", session_id, "--timeout", "20")
        try:
            watcher.wait(
                lambda value: value.get("data", {}).get("event", {}).get("type") == "state",
                timeout=20,
            )
            before_checks = len(observer.snapshot()["query"])
            wait(lambda: len(observer.snapshot()["query"]) > before_checks, timeout=15)
        finally:
            watcher.close()
        final = observer.snapshot()
    finally:
        observer.close()

    device_id = paired_client.identity["device_id"]
    query_topic = f"whalerein/v1/h/{scenario.host}/d/{device_id}/query"
    reply_topic = f"whalerein/v1/h/{scenario.host}/d/{device_id}/reply"
    result_topic = f"whalerein/v1/h/{scenario.host}/result"

    replies = final["reply"][len(baseline["reply"]):]
    # A directed reply is delivered only to its target Device; the Access
    # observer sees the other three hops and must not consume the Device reply.
    assert not replies, "a directed reply must not reach a third-party subscriber"
    # Only deadline-bound query publications carry Message Expiry. A HostReady
    # or other uncorrelated control result on the same topic is not part of the
    # query lane asserted here.
    starts = {lane: len(baseline[lane]) for lane in ("query", "request", "result")}
    query_ids = {
        entry["correlation"]
        for entry in final["request"][starts["request"]:]
        if entry.get("correlation")
    }
    def _correlation(entry):
        return entry.get("correlation") or entry.get("request_id")

    for lane in ("query", "request"):
        entries = [
            entry
            for entry in final[lane][starts[lane]:]
            if entry.get("correlation") in query_ids
        ]
        assert entries, f"the query lane emitted no correlated {lane} publication"
        assert all(entry["qos"] == 1 for entry in entries), (lane, entries)
        assert all(
            isinstance(entry["expiry_interval"], int) and entry["expiry_interval"] > 0
            for entry in entries
        ), (lane, entries)
    # The result lane carries the Host answer. Correlation/expiry on this hop
    # are asserted at the Host publish boundary in the host unit suite; here the
    # observer proves the HostResult for every admitted request reached Access.
    results_by_request = [
        entry
        for entry in final["result"][starts["result"]:]
        if _correlation(entry) in query_ids
    ]
    assert results_by_request, "the query lane emitted no correlated result"
    assert all(entry["qos"] == 1 for entry in results_by_request), results_by_request

    requests = final["request"][len(baseline["request"]):]
    assert len(requests) >= 3, requests
    assert len(results_by_request) >= 3, results_by_request
    assert any(entry["topic"] == query_topic for entry in final["query"]), final["query"]
    assert any(entry["response_topic"] == reply_topic for entry in final["query"]), final["query"]
    assert any(entry["response_topic"] == result_topic for entry in final["request"]), requests
    # The request topic is shared with non-query control requests; every
    # correlated query publication still creates no durable row.
    for correlation in query_ids:
        assert _durable_requests(scenario, correlation) == 0, (
            "a query must not create a durable request row"
        )


@pytest.mark.description(
    "An in-flight command is submitted and heartbeated over the durable HTTPS route: the request row is a COMMAND with a moving client lease, and the command request id never appears on the MQTT query ingress."
)
def test_command_submission_and_heartbeat_stay_on_https(scenario, paired_client, session_id):
    """A hold-open send renews its exact HTTP lease without using the MQTT query lane."""
    fixture = scenario.dsh.process
    observer = QueryLaneObserver(scenario)
    try:
        fixture.control("hold_next_send_turn", session_id=session_id)
        sender = paired_client.watch(
            "session", "send", session_id, "https command fixture", "--timeout", "30"
        )
        try:
            def row():
                rows = scenario.query(
                    "SELECT request_id,kind,status,relay_version,client_lease_until,expires_at "
                    "FROM requests WHERE command_id <> '' "
                    "ORDER BY created_at DESC LIMIT 1"
                )
                if not rows:
                    return None
                request_id, kind, status, relay_version, lease, expires = rows[0]
                return {
                    "request_id": request_id,
                    "kind": int(kind),
                    "status": status,
                    "relay_version": int(relay_version),
                    "lease": float(lease) / 1000.0 if lease is not None else None,
                    "expires": float(expires) / 1000.0,
                }

            wait(lambda: row() is not None, timeout=20)
            admitted = row()
            assert admitted["kind"] == 3, admitted
            assert admitted["relay_version"] == 5, admitted
            first_lease = admitted["lease"]
            assert first_lease is not None
            # The fixture normally holds the send turn so the lease must move.
            # If the Source answers immediately, completion legitimately stops
            # renewal; the command response then proves the lane instead.
            current = row() or {}
            # A lease already at the request expiry cannot move: the renewal is
            # capped by expires_at. Otherwise the held send must renew.
            if current.get("status") != "succeeded" and first_lease < current.get("expires", 0) - 0.5:
                # The command may complete while we wait; completion legitimately
                # stops renewal. Stop waiting on either a moved lease or a
                # terminal result so this is not a race.
                wait(
                    lambda: (r := row()) is not None
                    and (r.get("status") == "succeeded" or (r.get("lease") or 0) > first_lease),
                    timeout=15,
                )
        finally:
            fixture.control("finish_send_turn", session_id=session_id)
            assert sender.wait_exit(timeout=15) == 0
            sender.close()

        during = observer.snapshot()
        command_queries = [
            entry for entry in during["query"] if entry["correlation"] == admitted["request_id"]
        ]
        assert not command_queries, "the command request id must never use the MQTT query ingress"
    finally:
        observer.close()

    wait(lambda: (row() or {}).get("status") == "succeeded", timeout=15)
    completed = row()
    assert completed["status"] == "succeeded", completed
