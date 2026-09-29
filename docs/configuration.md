# Configuration and deployment packages

From any application directory:

```sh
snap build
snap build production
```

The positional argument defaults to `development`. Snap discovers the nearest
`snap.toml`, loads `.deployment/<environment>/config.toml`, and writes
`dist/<environment>/`. Development uses Cargo debug builds and development client
assets. Other environment names use release builds and production client assets;
their configuration must select `host.mode = "production"`.

```text
dist/production/
  server
  config.toml
  secrets.enc       # when secrets are required
  web/
  client.js         # when client.ts exists
  cli.js            # when cli.ts exists
  bridge.js         # when native/bridge.ts exists
```

Snap owns compilation, Wasm binding generation, frontend compilation and package
assembly. Apps supply `native/Cargo.toml`, `wasm/Cargo.toml`, `web/app.tsx` and
`web/index.html`. A conventional `web/server.tsx` may export an async default
function returning a filename-to-content map for server-rendered assets.
`[build]` in `snap.toml` accepts target selection only: `server`, `binary` and
`features`. It does not accept commands. Testy's existing host selects `server =
"local"`, `binary = "testy-web"` and `features = ["web"]`.

Packaging copies only produced artifacts, the selected config and encrypted
secrets, never the deployment directory wholesale. Failed builds retain the
preceding package. Builds for one environment cannot run concurrently.
The initial native target is the current Linux host architecture and libc. Build
in an environment compatible with the intended container image. Arbitrary
cross-compilation and fully static binaries are not promised.

## Runtime configuration

```toml
version = 1
[host]
mode = "production"
listen = "0.0.0.0:3850"
origin = "https://chatty.example.com"
data_dir = "/data"
database = "chatty-store.sqlite"
[app.oauth]
issuer = "https://authy.example.com"
client_id = "chatty"
client_secret_ref = "oauth.client_secret"
```

All servers use `snap-config` once at startup. They default to `config.toml` beside
the executable. `server --config PATH` selects a different file. Unknown fields,
unsupported versions and unsafe host modes fail startup. Relative paths resolve
against the config directory, never the working directory. `host.database` is a
filename within `host.data_dir`; `host.web_dir` defaults to adjacent `web/`.
Persistent data must stay outside the replaceable `dist/` tree. The build checks
paths as they will resolve from the final package, including existing symlink aliases,
and refuses unsafe storage before publishing or replacing any package.
Portable modules receive typed inputs, not loaders, files or environment access.

`server --check-config` validates shared and app-owned schemas and requires a bag
when the app declares secret references. It does not decrypt, open a database or
bind a listener. Builds invoke this check without the runtime key. Only startup
can verify the bag's contents. `server --migrate` explicitly migrates the configured
database without decrypting secrets. Normal startup never migrates automatically.

Production requires an HTTPS public origin and absolute data directory. Native
HTTP belongs behind a TLS reverse proxy; forwarding headers never choose the
trusted origin. Development requires a loopback backend. Production rejects
development-origin configuration. Testy's production host omits trusted debugger
routes. These controls do not establish a public security certification.

## Secrets

`secrets.enc` is an age file containing TOML. Nested string values resolve through
explicit typed `*_ref` fields. Plaintext secrets must not appear in config.toml.
Chatty's private authoring document contains:

```toml
[oauth]
client_secret = "a-strong-shared-client-credential"
```

Initialize each app/environment explicitly:

```sh
snap secrets init
# Privately edit .deployment/development/secrets.toml.
snap secrets seal

snap secrets init production
# Privately edit .deployment/production/secrets.toml.
snap secrets seal production
```

Create the environment's config first. Initialization refuses to overwrite existing
files. It creates private `secrets.key`, public `recipients.txt`, private
`secrets.toml` and encrypted `secrets.enc`. Sealing uses the public recipient,
without the private identity. The key and authoring file are gitignored and never
packaged. Exclude them from Docker build contexts too. Use a dedicated key pair per
app/environment.

The deployer injects the age private identity as `SNAP_MASTER_KEY`, the only server
configuration environment variable. Rust's `age` library decrypts directly; no
external executable is required. Missing references, invalid values, wrong keys
and damaged ciphertext fail before serving. Errors do not echo parser source or
values. Secret wrappers are redacted and non-serializable; explicit exposure to an
external implementation still requires care. Encryption does not protect a
compromised running process.

Authy's `[app]` contains `clients`, optional `app_domain`, `auto_approve_domain` and
optional `cookie_key_ref`. Clients declare `id`, `name`, `origin` and optional
`client_secret_ref`. Its development bag uses `clients.chatty` and
`clients.factorio`. Give each relying party the matching value in its own
`oauth.client_secret`. Generated cookie/signing keys remain instance data in SQLite.

Factorio adds `[app.repository]` with its existing typed repository settings, and
optional `[app.tools]` for `bun`, `opencode`, `model` and `bridge`. The bridge defaults
to packaged `bridge.js`; an explicit path resolves against config.toml. Set its
installation-specific repository/resource paths before starting it.

Old `.snap/chatty.env` and env-var configurations are not implicitly imported. Copy
credentials into the private authoring files, seal each bag, and preserve existing
databases and generated keys.

## Development and containers

`snap dev` reads `.deployment/development/config.toml`. Optional `[dev].listen`
controls the frontend separately from the loopback backend. The Rust CLI writes an
explicit config for each backend generation with discovered origins and allocated
ports. It reads the conventional development `secrets.key` unless `SNAP_MASTER_KEY`
is supplied. Servers never discover that key file. `snap dev --config PATH` supports
explicit installations and disposable fixtures. Config, `secrets.enc` and
`secrets.key` changes are revalidated and applied automatically. Invalid changes
retain the preceding generation. Startup and replacement never migrate a database.

The CLI owns filesystem watching, native/Wasm builds, readiness, publication and
process-group shutdown. Apps use their `[build]` target selection for development
too; `snap.toml` no longer accepts `[dev].commands`. React/CSS changes use Vite HMR.
Rust changes publish matching native and Wasm artifacts, then reload browsers.
Compilation failures keep the current backend running. Replacement stops the old
backend before opening its database; startup failure restarts the preceding one.

`tools/cli/web-dev.mjs` is the framework-owned Node adapter for Vite, React refresh,
generated bindings and the checked proxy policy. It receives no deployment key.
Checkout edits to this adapter restart only Vite, with rollback if it cannot start.
Changes to the Rust CLI or its embedded build helper require rebuilding and
relaunching `snap dev`. Network-address changes require relaunching it too.

Use `host.origin` for the stable public URL and the OAuth issuer/client origins for
peer URLs. HTTPS development requires a loopback frontend behind the proxy. HTTP
development can explicitly select `[dev].listen = "0.0.0.0:<port>"` for LAN/Tailscale.

```dockerfile
FROM debian:bookworm-slim
WORKDIR /app
COPY dist/production/ /app/
CMD ["/app/server"]
```

The deployer supplies `SNAP_MASTER_KEY` and mounts durable storage at the configured
data directory. Provision/migrate that storage explicitly. Worktree setup, key
custody and cloud deployment remain operator concerns. A later cloud host can
supply the same typed inputs through bindings without using this filesystem layout.

Root tooling can use `snap build --project apps/chatty`. Normal app-local use needs
no paths. Development/test tooling uses the framework's `--web-only` build with a
generation output directory, not app-specific build scripts.
`snap build --output DIRECTORY` builds a complete private development generation
through the same native/Wasm/client pipeline. The supervisor writes its explicit
runtime config and bag afterward. `--web-only` limits this tooling output to web
assets. Wasm declarations accompany JS bindings; static imports use `@snap/wasm`
with the app's TypeScript path mapped to its generated development bindings.
