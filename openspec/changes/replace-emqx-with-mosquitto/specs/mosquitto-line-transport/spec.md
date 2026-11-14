# Mosquitto Line Transport

## Purpose

Operate one independent MQTT line with Mosquitto and preserve existing
WhaleRein clients and transport behavior.

## ADDED Requirements

### Requirement: Local broker policy with existing identities
The line SHALL serve existing MQTT 5 clients at the current endpoint and use
the existing TLS identity, device credentials and topic contract. The broker
SHALL enforce publish, subscription, outgoing delivery and Will policy locally.

#### Scenario: AC-06 MQTT identity and ACL are enforced on each broker operation
- **WHEN** an existing Core, Host client or internal Access client connects and exercises its authorized MQTT operations
- **THEN** its established identity and client ID binding determine its own permitted topic directions and operations
- **AND** foreign topics, unauthorized wildcard or shared filters, unauthorized Wills and control topics are denied
- **AND** publish, subscribe and delivery produce zero broker-to-Access authorization requests
- **AND** revocation blocks delivery through a subscription created before revocation and invalidates a superseded connection

### Requirement: Transport and recovery compatibility
The line SHALL preserve the existing MQTT QoS and application transport
semantics, including framing, encrypted payload handling and recovery.

#### Scenario: AC-07 Stream and committed event recovery survive broker replacement
- **WHEN** an existing client exchanges live chunks, committed event parts, transport receipts, terminal events and recovery requests through the new line
- **THEN** the existing logical framing, size bounds, receipt meaning and committed event recovery apply
- **AND** reconnects, duplicate deliveries and missing parts are handled according to the existing client contract

### Requirement: One independently deployed line
The replacement deployment SHALL run one active Access and one Mosquitto per
line, with PostgreSQL state available from an external server. The deployment
SHALL activate external traffic only after the broker can enforce current
policy and Access is ready to forward.

#### Scenario: AC-08 A hard cut preserves existing enrollments
- **WHEN** the operator drains and replaces the EMQX line while retaining PostgreSQL state, MQTT address and TLS identity
- **THEN** existing devices reconnect with their stored credentials, resume eligible work and remain subject to committed revocations
- **AND** the release can be reverted as a complete deployment while preserving current durable grants and command state

#### Scenario: AC-09 Performance is measured on the target line
- **WHEN** the Access and Mosquitto line is measured on 2 vCPU and 4 GiB with separate PostgreSQL, 2,000 mostly idle TLS MQTT connections, active streaming and a reconnect and catch-up phase
- **THEN** the recorded results include actual connection counts, workload parameters, latency distributions, authorization queries, throughput, connection and delivery errors, CPU, memory and bandwidth
- **AND** acceptance uses the functional and authorization requirements and records latency without a numerical pass threshold
