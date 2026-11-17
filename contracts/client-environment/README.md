# Client environment contract

One canonical contract drives Rust Core and the Node Host runtime. The
editable sources are in this directory; generated distribution copies are
byte-identical and are never edited by hand.

| File | Meaning |
| --- | --- |
| `catalog.json` | Built-in official environments: service identity, mode, API/issuer origins, native registration, trust defaults and the environment token the service metadata must report. |
| `cases.json` | Golden corpus: per fixture, the normalized document, the stable error code, and expected resolution results. |
| `fixtures/valid/*.toml` | Valid version-1 documents. |
| `fixtures/invalid/*.toml` | Documents that must be rejected without rewriting. |

Both runtimes must produce the same normalized result or the same stable
error code for every corpus entry. A test may not relax the corpus to match an
implementation.

## Document schema (version 1)

```toml
version = 1                              # required, integer, exactly 1
environment = "production"               # optional; validated identifier

[environments.<name>]
label = "Display label"                  # optional
mode = "account" | "standalone"          # required for custom entries
api_origin = "https://cloud.example.com" # account only, https
issuer = "https://auth.example.com/oidc" # account only, https, distinct origin
native_client_id = "whalerein-cli"       # account only
environment = "..."                      # never accepted from users

[environments.<name>.tls]
ca_provider = "system" | "mkcert" | "file"
ca_file = "/absolute/path.pem"          # required iff provider is "file"
```

Rules enforced on both runtimes:

- Only one file loads. `--config` / `config_file` beats `WHALEREIN_CONFIG`,
  which beats `~/.whalerein/config.toml`. No merge, no current-directory
  discovery.
- Built-in names (`local`, `staging`, `production`) may only be specialized
  with `label` and `tls`. Any authority or mode field is
  `authority_override`.
- Custom names are `[a-z0-9][a-z0-9-]{0,63}` and may not equal a built-in
  name.
- Account mode requires all three account fields; standalone mode forbids
  them. A hostname never implies a mode.
- Managed Access/MQTT routes are not schema fields. `mqtt_url`,
  `access_url`, `region`, broker keys and similar fail as `unknown_field`.
- `ca_provider` defaults to `system`; built-in `local` defaults to `mkcert`.
  `mkcert` is only accepted for built-in `local`. `ca_file` is required for
  provider `file` and resolves relative to the configuration file's parent
  directory; `~` is expanded once.
- Unknown fields, duplicate declarations, missing/non-integer/future versions
  are rejected. Absolute paths and URLs are validated without inferring
  anything from a hostname.
- Values are literal strings. No shell, JavaScript, environment-variable
  interpolation, includes, `extends` or remote synchronization.

## Stable error codes

`invalid_syntax`, `duplicate_key`, `unsupported_version`, `unknown_field`,
`invalid_identifier`, `invalid_url`, `invalid_tls`, `invalid_mode`,
`invalid_mode_fields`, `authority_override`, `unknown_environment`,
`missing_config`, `unreadable_config`, `revision_conflict`, `import_conflict`,
`import_unsupported`, `import_storage_unavailable`.

Validation order is fixed so both runtimes report the same first error:

1. read → `missing_config` / `unreadable_config`
2. parse → `invalid_syntax` / `duplicate_key`
3. version → `unsupported_version`
4. top-level shape and selection identifier → `unknown_field` /
   `invalid_identifier`
5. environments table shape → `unknown_field` / `invalid_identifier`
6. each environment in ascending name order: unknown fields, built-in
   specialization, mode fields, URLs, trust
7. selection resolution → `unknown_environment`

Errors carry a stable code plus a short non-secret message and optional
environment name. They never echo file bytes or URL credentials.

## Normalized shapes

`cases.json` `document`:

```json
{
  "version": 1,
  "environment": null,
  "environments": {
    "company": {
      "id": "company",
      "mode": "account",
      "label": "Company Cloud",
      "api_origin": "https://cloud.example.com",
      "issuer": "https://auth.example.com/oidc",
      "native_client_id": "whalerein-cli",
      "environment": null,
      "tls": {"ca_provider": "system", "ca_file": null}
    }
  }
}
```

- `environment` is `null` when the file did not declare a default.
- `id` in a normalized *document* entry is the key the file declared (for
  built-ins that is the reserved name, for example `production`). A resolved
  `definition.id` is the service identity from the catalog (`official-*`) for
  built-ins and the custom environment name otherwise.
- `environment` inside an entry is the service environment token expected from
  live metadata; it is `null` for custom entries until metadata is observed.
- Every field is present; absent values are `null`. Entries serialize in
  ascending name order.
- `tls` is `null` when the file did not declare trust; resolution applies
  defaults.

`cases.json` `resolution.expect`:

```json
{
  "environment": "company",
  "selection_source": "file",
  "definition_source": "file",
  "definition": { "...effective merged definition, same fields as above..." }
}
```

`selection_source` is `invocation`, `file` or `default`.
`definition_source` is `builtin` (catalog authority with optional label/trust
specialization) or `file` (custom entry). Resolution never falls back to
production when the selected name is unknown.

## Packaging

- Rust embeds `catalog.json` and `cases.json` with `include_str!` from this
  canonical directory, so binaries resolve built-ins outside the source tree.
- `host/runtime/src/configuration/catalog.json` and
  `host/runtime/src/configuration/cases.json` are generated copies produced by
  `host/runtime/scripts/sync-client-environment-contract.mjs` and shipped by
  the package `files` list. A repository test fails when a generated copy
  differs from the canonical file.
- `tests/integration/Dockerfile` copies `contracts/client-environment` so
  container builds do not depend on a Cloud checkout or a repository-relative
  runtime lookup.
