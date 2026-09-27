# Stock Codex account router

This experimental sidecar keeps the shared stock server's phone/control login
stable while an owner selects a separate execution sign-in for an opted-in task's model
requests. Selection is manual and happens between turns. Future automatic
selection can use the same interface; this package does not rotate accounts.
The owning work item and live acceptance record are
[codex-lab #979](https://github.com/cbusillo/codex-lab/issues/979).

The host target is macOS or Linux with a local Unix control socket. The stock RPC
adapter requires app-server 0.157.1. A different version needs qualification
before changing that guard. Synthetic qualification is not proof that real
encrypted reasoning or compacted history works across accounts, or that the
phone can reach the host. Those remain live acceptance gates in #979.

## Setup

Use fresh device logins for execution accounts with available model allowance.
The phone/control account may also be an execution choice: enroll it with its own
fresh device login in an execution home. The control server and execution worker
retain separate sign-ins and each owns its own token refresh and persistence.
Labels such as `execution-a`
are local names for execution slots; they do not mean the owner's primary account.
Never copy the main server's or retired Lab's credential files. The main server
must already be running with its normal control login. From this directory:

```sh
uv sync --locked --group dev
uv run codex-account-router login execution-a
uv run codex-account-router configure
uv run codex-account-router serve execution-a
```

One control account and one distinct execution account are enough to test
separation. To add another execution choice, enroll an unused label with
`uv run codex-account-router login execution-b`. Existing routed tasks can keep
running during this new-label login. Once those tasks are idle, stop the router
and start it with `uv run codex-account-router serve execution-a execution-b`.
Complete each stock device sign-in as the corresponding execution account.
`configure` adds only the `account-router` provider through stock's versioned
config RPC. It preserves the default provider and refuses a conflicting existing
definition. After installing the same package into a different environment,
rerun `configure` from that installation: it can update the interpreter path when
every other managed provider/auth setting is unchanged.
Its supported auth command reads the current control access token
through the local socket and delivers it over a private credential pipe; it does
not create another credential store or force a refresh. The provider uses this
command instead of `requires_openai_auth`, so the control account's exhausted
allowance does not drive the TUI's automatic model selection.
The configured command uses this package's Python environment, which must remain
available while routed tasks exist. Do not run the credential helper as a logging
or diagnostic command: its stdout is a bearer token for stock to consume.
`serve` runs in the foreground, binds model HTTP to `127.0.0.1:41979`,
and exposes administration only through a private Unix socket. It must stay
running while opted-in tasks execute. The first listed execution account supplies
the model catalog, which stock requests without a task identity.

In another terminal:

```sh
uv run codex-account-router start execution-a "Describe this project" --cwd /absolute/path/to/repository --name "My task"
uv run codex-account-router status
uv run codex-account-router select TASK_UUID execution-b
uv run codex-account-router resume TASK_UUID
```

`start` creates and names a task in the existing shared server, submits the given
first request, then opens stock `codex resume`
with an explicit `--remote` Unix socket, preventing a fallback to another owner.
It also selects `account-router` in the TUI's local configuration so the client
uses the same provider semantics as the owning task.
The TUI uses the local server's user config directory, discovered through config
RPC, so an explicitly selected isolated control socket loads that host's provider.
Use the router's `resume` command when reconnecting: it checks the task's provider
and supplies that same client configuration without creating a new turn.
If the request is omitted, the command prompts for it before creating the task.
Stock defers empty-task history, so the first request must be persisted before
another client can attach; the launcher verifies that same-server resume succeeds.
Without `--name`, the task initially uses the working directory's name.
Its creator connection stays subscribed while the TUI is open;
it never answers the TUI's approval or tool requests. The account panel in stock
Codex still describes the control account. Router status and selection receipts
identify execution labels separately. `lastRequest.httpStatus` is an HTTP receipt,
not a claim that the model turn completed; task state comes from the owning server.
The phone list contains the most recent 100 explicitly registered tasks; inherited
children are omitted. A fork needs an explicit `select NEW_TASK_UUID LABEL` before
it appears there. Forks do not inherit a selection automatically.
Start each additional routed task with `codex-account-router start`. Stock TUI
`/new` uses the host's default provider and can run on the phone/control account;
the launcher covers only the specific task it opens. Do not treat `/new` as a
routed task when the host default is unchanged.

All global options precede the command: `--data-dir`, `--control-socket`, `--codex`,
and `--port`. Defaults use the current `CODEX_HOME` (or `~/.codex`), a private
`account-router` subdirectory, and the stock `app-server-control` socket.
Use the same options for every command. Configuration and serve ports must match.

## Phone selector

Use ordinary SSH over Tailscale. The phone and host must be on the same reachable
Tailscale network, TCP 22 must be allowed, and the phone needs the Mac's SSH login.
This does not require the Tailscale SSH server feature or public port forwarding.

Build a native Shortcut with these actions:

1. Connect Tailscale using its Shortcuts action.
2. Run Script Over SSH on the host's full MagicDNS name ending in `.ts.net`,
   invoking the installed router command with `phone-list`.
3. Split the result by new lines, then Choose from List. Each row names an idle
   task and an execution email plus its local label; its UUID distinguishes tasks
   with the same name. One Shortcut lists every enrolled, served execution choice.
4. Show Alert with the chosen row and a Cancel button to confirm the target.
5. Run Script Over SSH with the fixed command `codex-account-router phone-select`.
   Set this action's **Input** to the chosen item, then Show Result. The receipt
   identifies the account selected for the next turn and confirms the phone login
   stays unchanged; selection itself does not start a model request.

Use an absolute installed command path in SSH because its PATH can differ from
Terminal. Pass the choice through the SSH action's Input field (stdin), never
interpolate task names into shell code. The selector compares it against a fresh
inventory and refuses a stale, busy, or unavailable choice. The JSON `status` and
`select TASK_UUID LABEL` commands remain available for other clients.
The owner enters SSH authentication in Shortcuts.
Test from cellular with Wi-Fi disabled, including while the execution account is
limited. Tailscale's hostname-triggered On Demand rule matches `.ts.net`; a short
hostname may not activate it. See the official [Shortcuts actions](https://tailscale.com/docs/features/mac-ios-shortcuts)
and [On Demand rules](https://tailscale.com/docs/features/client/ios-vpn-on-demand).

## Independent macOS startup

`service` supervises an already enrolled isolated phone host and its router.
It locks that home, verifies the control identity, and restores registered tasks
before enabling Remote. It keeps observer subscriptions for the latest 100 root
registrations, discovers new ones every five seconds, and releases older ones.
These observers prevent a phone model override from cold-resuming with stock's
other default provider. They never start turns or answer approval/tool requests.
Use the router's `start` command for new routed tasks.

Create a mode-0600 configuration outside the source checkout, for example
`~/.codex/account-router/service.json`:

```json
{
  "codex": "/absolute/resolved/pinned-release/bin/codex",
  "data_dir": "/absolute/private/account-router",
  "control_home": "/absolute/private/account-router/phone",
  "control_account_id": "account/read workspaceRouting.chatgptAccountId",
  "accounts": ["execution-a", "execution-b"],
  "port": 41979
}
```

Use the intended host's existing RPC account identifier, never a token. Its
control home must be a direct child of the data directory, with an existing
stock file login and the configured provider using its `host.sock` and router
port. All paths must be absolute and resolved; pin the qualified stock release.
Install a built wheel with `uv tool install`, then render from that installation:

```sh
umask 077
mkdir -p "$HOME/Library/LaunchAgents"
"$HOME/.local/bin/codex-account-router" launch-agent "$HOME/.codex/account-router/service.json" > "$HOME/Library/LaunchAgents/com.codex.account-router.plist"
plutil -lint "$HOME/Library/LaunchAgents/com.codex.account-router.plist"
# After reviewing paths and stopping the idle foreground pilot:
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.codex.account-router.plist"
launchctl print "gui/$(id -u)/com.codex.account-router"
# Stop for maintenance or rollback:
launchctl bootout "gui/$(id -u)/com.codex.account-router"
```

launchd starts it at user login and restarts failures with a delay. Screen lock
is supported; logout stops it and a reboot requires login. Check router `status`
before planned restart because active turns can be interrupted. Verify Remote,
the account menu and same-ID resume after installation. Logs rotate under
`data_dir/logs/` (four files of roughly 1 MiB each). Rollback unloads this agent,
removes only its plist, and restores the retained wheel/foreground setup; preserve
all account homes, provider configuration and history. Other CLI hosts' sessions
remain on their original host and are not migrated by this service.

### Attach to the everyday daemon

An installed isolated service does not expose another daemon's sessions. To use
the existing everyday daemon, set `"mode": "attach"` and `control_home` to that
daemon's existing Codex home in the service configuration. Set the expected
`control_account_id` from that daemon's `account/read` response after its normal
stock sign-in. The socket is `control_home/app-server-control/app-server-control.sock`.
The service verifies the identity before enabling Remote. It never starts, stops,
replaces or writes credentials for the existing daemon. At login it waits for the
daemon; launchd retries if the daemon is not available yet. The stock daemon keeps
its normal startup/update ownership; an unqualified stock version is refused.

For a transition from an isolated pilot, first let its turns finish, stop its
service, and preserve its configuration and selection registry for rollback.
Start the everyday service with its own empty `selection.json`, retaining the
existing enrolled account homes. Do not copy credentials or conversation history.
Configure the named provider through `configure` with the everyday socket before
starting adoption. The phone account and everyday daemon's control sign-in must
match; sign in and pair through stock's normal flow when ready.

Let each existing task finish its turn, then exit its other harnesses normally.
An idle client still owns a subscription, so merely waiting between turns does
not make a different provider effective. Adopt the saved task by its original ID:

```sh
codex-account-router adopt TASK_UUID execution-a
codex-account-router resume TASK_UUID
```

Adoption refuses busy tasks and provider changes while another client remains
attached. It starts no model turn, preserves the task ID and history, selects the
execution account and retains a service subscription before returning. The same
task can then be opened locally or on the paired phone. New routed tasks still
use `start`; ordinary stock `/new` continues to use the host default provider.
Changing only the control login does not route existing or new ordinary tasks.

## Ownership and failure behavior

Stock app-server workers own execution refresh and persistence in separate
0700 homes. The router receives access tokens in memory through owner-local
`getAuthStatus` and validates `account/read` routing and account identity. It does
not read refresh tokens. Kernel locks are inherited by worker/login processes,
so an exiting parent cannot admit a concurrent credential writer prematurely.
Execution identities must differ from one another. An execution identity may
match the control account when enrolled through a separate fresh login; never
copy or share the control server's credential file with a worker.
Read-only control connections reconnect to the same account after a disconnect;
dead credential workers can restart while retaining their account lock and binding.
This first qualification targets personal ChatGPT accounts. Identity comparison
uses workspace IDs, so separate seats in one Business/Enterprise workspace are
not supported. A worker that times out without disconnecting needs an operator
restart: interrupt its affected turn, then restart the router when routed tasks
are idle. A timeout does not automatically terminate a possibly running refresh.

The HTTP caller must supply the current control token; comparisons never force a
control refresh. Execution 401s get one worker refresh and one retry. Rejected
execution tokens are then quarantined until credentials change or the router is
restarted. Every exposed authentication failure is 403, preventing stock from
refreshing the control login in response. Usage limits pass through as 429.

Account pins survive router restart. Active targets reject selection; stock's
idle-after-error status permits recovery from a usage limit. Unknown tasks fail
closed. Children inherit only through a parent relationship verified with stock,
and follow the parent's selection on each new turn unless selected explicitly.
Model requests support `/responses`, `/responses/compact`, and catalog `/models`;
other backend endpoints fail explicitly. Bodies and SSE bytes retain compression,
redirects are refused, and a partial response is never replayed by the router.

Stop the foreground router before re-enrolling a label. Stock login must return
to that label's original account; a changed identity fails closed. To roll back,
stop the router and its owned workers after the canary is idle. Preserve execution
homes and the provider definition while tasks still depend on them. Unrelated
tasks and the shared phone server keep their existing provider and ownership.

## Development checks

```sh
uv run python -m unittest discover -s tests
uv run ruff check .
uv run ruff format --check .
```

On macOS, qualify the installed stock binary against local fake OAuth/model
servers and fresh synthetic homes. Stock subprocesses can contact only loopback.
The output directory must be new and is retained for inspection:

```sh
uv run --group qualification python tests/qualify_stock.py \
  --codex /absolute/path/to/codex --output /absolute/path/to/new-evidence-directory
```

The probe covers configuration idempotence, normal turns, a usage limit, manual
selection, bounded execution refresh, control credential preservation, history
continuity, and cold resume on the same task. Real backend reasoning/compaction,
same-server TUI attachment, OAuth enrollment, and phone use need separate live
acceptance. It never accepts real auth files as fixture input.
