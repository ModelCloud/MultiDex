# BGR: role-constrained brain, grunt, and reviewer execution

Status: **experimental first backend slice implemented**. The next section is
the actual supported wire contract. Later sections retain the broader design;
their conceptual fields are not automatically supported configuration/API fields.

Companion: [Omnigent orchestration design](https://github.com/ModelCloud/omnigent/blob/design/brain-executor-reviewer-roles/docs/BRAIN_EXECUTOR_REVIEWER_DESIGN.md).

## Implemented backend contract (version 1)

Opt in with `initialize.capabilities.experimentalApi=true`. RPCs use the native
JSON-RPC transport, camelCase field names, and ordinary per-thread request
serialization. This is a trusted host bridge, not a public multi-tenant identity
service: the host controls thread creation, binding, and membership. Models
cannot select their reporting sender/recipient/epoch; the backend derives these
from the bound thread and original query. A query's `senderAgentId` is checked
against the bound `sourceThreadId`, not trusted on its own. Omnigent must
authenticate/authorize its callers before allowing host RPCs.

| RPC / notification                  | Parameters / result                                                          | Meaning                                                                                                                           |
| ----------------------------------- | ---------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `bgr/capabilities/read`             | `{}` -> capabilities below                                                   | Negotiate before starting managed threads                                                                                         |
| `bgr/thread/bind`                   | `{threadId, identity}` -> `{snapshot}`                                       | Bind a fresh, delegation-disabled normal thread; exact repeated binding is idempotent                                             |
| `bgr/message/send`                  | `{sourceThreadId, threadId, envelope}` -> `{messageId, accepted, duplicate}` | Queue a validated `status_query`; returns without waiting for worker output                                                       |
| `bgr/thread/inspect`                | `{threadId}` -> `{snapshot}`                                                 | Inspect/reconcile an already live attempt; never cold-starts a replacement                                                        |
| `bgr/thread/checkpoint`             | `{threadId, identity}` -> `{snapshot}`                                       | Validate the complete identity and flush the native rollout; not an atomic durable BGR checkpoint                                 |
| `bgr/thread/cancel`                 | `{threadId, identity}` -> `{snapshot}`                                       | Fence the assignment immediately and submit native interruption; acknowledgement does not prove every external process has exited |
| `bgr/message` (server notification) | `{threadId, envelope}`                                                       | Typed status reply emitted by the native worker tool; also retained in `inspect`                                                  |

Capabilities are `version:1`, `roles:["brain","grunt"]`,
`statusScopes:["now"]`, `messageKinds:["status_query","status_reply"]`,
`liveReattach:true`, `maxQueriesPerAttempt:128`, and the exact `reportTool`
dynamic-tool specification. `durableMailbox`, `restartRecovery`,
`reviewerSandbox`, `crossHostDelivery`, and `taskDispatch` are all **false**.
Other declared message kinds and scopes are reserved, not implemented. Neither
progress/result envelopes nor review/publication jobs are accepted by send.
Ordinary native turn/item/usage events remain available, but `turn/completed`
is not an attestation of task success.

### Start, bind, query, report

1. Discover capabilities. Create each role via ordinary `thread/start` with its
   own resolved provider/model/reasoning/speed settings. This slice does not
   introduce model selection/remapping or a new variant configuration field.
   Set native feature overrides `features.multi_agent=false`,
   `features.multi_agent_v2=false`, `features.defer_mailbox_preemption=true`.
   Include the returned `reportTool` in each grunt's `dynamicTools` unchanged.
2. Before any user turn/history, bind
   `{teamId,agentId,sessionId,taskId,attemptId,epoch,role}` with host-owned IDs
   and a positive `epoch`. Omnigent's lowercase 32-hex IDs are preserved;
   `agentId` is an opaque ID, not the display label `brain`/`grunt-...`.
   Reviewer binding is rejected, not silently
   downgraded. Team membership and uniqueness across threads are host-owned.
   A bound thread cannot be rebound to a new task/attempt; allocate a new thread.
3. Start the host-authorized task using native `turn/start`. Arbitrary task
   text is **not** accepted as BGR message payload. Native nested delegation
   tools are disabled by the required feature configuration; host-provided
   tools, shell policy, credentials, and worktree ceilings remain host-owned.
4. Send a query from the bound brain thread, targeting the complete worker
   identity. Example envelope (optional fields may be omitted):

   ```json
   {
     "version": 1,
     "messageId": "11111111111111111111111111111111",
     "senderAgentId": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
     "recipientAgentId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
     "identity": {
       "teamId": "cccccccccccccccccccccccccccccccc",
       "agentId": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
       "sessionId": "22222222222222222222222222222222",
       "taskId": "dddddddddddddddddddddddddddddddd",
       "attemptId": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
       "epoch": 1,
       "role": "grunt"
     },
     "kind": "status_query",
     "correlationId": "ffffffffffffffffffffffffffffffff",
     "payload": { "scope": "now" }
   }
   ```

   The receipt is process-local queue acceptance, not history commitment or
   durable delivery. Native `Op::InterAgentCommunication` targets the selected
   thread (each normal managed thread has native `/root` identity). The
   existing input queue and deferred mailbox-preemption policy deliver at a
   safe boundary, without a parallel turn, forced steering, context reset, or
   killing a long-running tool. If idle, normal mailbox handling can start the
   next turn of the same assignment. There is no second mailbox polling loop.

   The adapter maps snake_case host fields to these camelCase wire fields;
   `messageId` is the original host message/idempotency ID, never a separate
   server envelope ID. Every `inReplyTo` refers to that original ID. The host
   ledger must keep the separate mapping from `identity.sessionId` (Omnigent
   logical session ID) to RPC `threadId` (MultiDex native thread UUID). Neither
   binding nor replies overwrite the logical session ID. `sourceThreadId`
   selects the native brain thread; notification `threadId` selects the native
   reporting worker. Role display names and membership uniqueness remain
   host-owned. The backend accepts bounded ASCII IDs, including host 32-hex IDs.

5. The worker calls `bgr_report_status` with
   `{messageId,inReplyTo,payload:{currentAction,completed,pending,blockers,
headSha?,dirty?,coverageLimits}}`. The backend constructs the reply's
   identity, sender, recipient and original correlation, emits `bgr/message`,
   acknowledges the tool, and lets the worker continue its assignment. Evidence
   fields are worker self-report, not repository attestation. Omnigent consumes
   this event and owns delivery into the brain's durable inbox; MultiDex does
   not schedule or block the brain waiting for it.

Identical query/reply retries are idempotent; ID reuse with different content
fails. Replies may arrive out of order and retain `inReplyTo`, correlation, and
attempt identity. Cancellation and all target-identity fields fence late
reports. A sender must be a live bound brain in the same team and epoch.
Envelope/report bodies are bounded to 32 KiB, and a binding retains at most 128
queries (including answered queries); it does not evict deduplication history.
At the limit, the host must reconcile and explicitly finish/replace the
attempt rather than assume delivery succeeded.

### Recovery limits

`snapshot` is `{identity,cancelled,sequence,pending,replies}`; sequence counts
accepted queries, fresh replies, and cancellation. Persist this compact
metadata in Omnigent, not model reasoning/context. Native rollout flush does
not create the host's phase/summary/head-SHA/artifact-reference checkpoint;
those remain Omnigent ledger metadata. It also does
not persist the BGR binding, queue, or dedup state. A live inspect plus existing
native thread subscription can reattach to the same loaded attempt. After
process loss or unload, inspect fails; ordinary `thread/resume` does not restore
the binding or role ceiling. Do not resume a managed attempt as an ordinary
session. Fence the old attempt externally and allocate a clean replacement
with a new native session/attempt identity. Reviewer retries likewise require
fresh independent sessions once reviewer enforcement is implemented.

Remaining stages: scoped history coverage, role-constrained job dispatch,
reviewer isolation, typed progress/results, durable replay/acknowledgement,
restart recovery and broader configuration-ceiling enforcement. These are
deliberately not advertised by this first slice.

## Purpose and boundary

Support Omnigent's BGR (brain-grunt-reviewers) mode, a separate opt-in choice
alongside Polly in the Omnigent server/host UI. BGR has one persistent planning
brain, task-scoped grunts, and one or two independently configured reviewers.
MultiDex supplies role-constrained execution and reports evidence; Omnigent
owns profile configuration, the task graph, integration/publication, review
scheduler, PR comments, and delivery back to the brain.

The main brain may itself use MultiDex, but there is still only one orchestrator.
Omnigent alone schedules BGR jobs and enforces the dynamic in-flight grunt cap;
MultiDex must not create a second scheduler, static worker pool, or pre-spawned
grunts. Standalone MultiDex orchestration/PR automation is a separate future
proposal.

| Role     | MultiDex behavior                                                                                                       | Ownership outside this backend                                      |
| -------- | ----------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------- |
| Brain    | Normal persistent session with resolved provider/model/variant/speed/reasoning, orchestration tools exposed by the host | Omnigent supplies durable lifecycle and publication operations      |
| Grunt    | Execute a scoped contract; return changes, checks, and blockers                                                         | Brain makes design decisions and accepts/integrates work            |
| Reviewer | Fresh session; inspect exact revision; report findings without source edits                                             | Omnigent starts each round and publishes results; brain adjudicates |

Bounded grunts can make routine implementation choices but must return
unsettled design questions. Lower reasoning is a supported model option where
available, not a requirement to suppress error detection. Reviewers need enough
reasoning to perform independent analysis.

## Reuse existing primitives

- [Agent tools](../codex-rs/core/src/tools/handlers/multi_agents_spec.rs) already
  expose provider/model/reasoning controls in applicable tool surfaces.
- [Child configuration](../codex-rs/core/src/agent/child_config.rs) and
  [agent control](../codex-rs/core/src/agent/control/) are integration points for
  inherited restrictions and managed children.
- [Review tasks](../codex-rs/core/src/tasks/review.rs) already start a one-shot
  thread without initial history and disable delegation tools for that review
  path. This is a useful primitive, not evidence that the entire proposed
  post-push, multi-provider lifecycle already exists.
- Existing mailboxes and cancellation mechanisms should carry job completion
  and interruption rather than adding an independent polling loop.

Preserve upstream-owned conversation/context construction and formatting.
Do not invent a fork-specific transcript format, rewrite compaction payloads,
or change existing provider compatibility semantics to implement these roles.
Use existing thread/context entry points and capability/permission abstractions.

## Proposed capability handshake

Before launching a managed job, the adapter negotiates a protocol version and
capabilities for explicit provider/model/variant/speed/reasoning selection,
fresh context, role tool restrictions, isolated workspace access, structured
results, cancellation, and durable job identity/reconciliation.

An incompatible backend must reject the managed role with an actionable error.
Do not silently downgrade to a normal coding session or prompt-only read-only
mode. Old clients and ordinary interactive MultiDex sessions retain their
current behavior.

Omnigent resolves each capability-aware profile against the catalog and passes
the selected provider/model, supported variant, speed, reasoning, execution-
compatible harness, and configuration revision in the job snapshot. The UI
offers only supported values, validates combinations, and rejects invalid
explicit choices rather than silently remapping them. Harness is execution
compatibility, not a substitute for model/provider settings. Grunt jobs use the
configured default unless the brain explicitly assigns a permitted profile
within user policy; there is no implicit task-based auto-routing. Optional
per-grunt profile defaults do not require configuring a static pool.

Secrets stay in the existing credential mechanism and never enter job payloads,
logs, result artifacts, or review checkout contents. Do not add a second
credential format as part of this feature.

## Conceptual job and result contracts

| Job field                                | Semantics                                                                                               |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| Protocol version, job ID, attempt ID     | Version compatibility, correlation, retry/reconnect reconciliation                                      |
| Owner/parent ID, task or review-round ID | Trusted ownership; no arbitrary cross-session result delivery                                           |
| Role                                     | Brain, grunt, or reviewer; runtime-enforced capability ceiling                                          |
| Display name                             | Runtime-assigned role-prefixed name, visible in UI and reports                                          |
| Selection snapshot                       | Provider/model, resolved variant/speed/reasoning, execution-compatible harness, profile/config revision |
| Repository/workspace                     | Repository identity, exact starting/head SHA, isolated workspace identity; base/merge-base for review   |
| Context mode                             | Persistent brain; task-scoped grunt continuation; fresh reviewer                                        |
| Contract                                 | Objective, plan, allowed scope, acceptance criteria, exact checks, escalation conditions                |
| Limits                                   | Time/resource budget, output limits, cancellation identity                                              |

Omnigent persists and enforces the dynamic max-concurrent-grunt cap across brain
turns and restarts. The cap is an in-flight upper bound, not a pool size: the
brain starts only needed grunts, excess work is queued or rejected, and
reviewers do not consume the cap. Lowering the cap never terminates active
grunts; Omnigent blocks new starts until the in-flight count falls below the
new cap. MultiDex executes explicitly dispatched jobs and owns no second
scheduler or concurrency pool.

Results echo the job identity, role, display name, resolved selection, and
configuration revision, then provide:

- Status: completed, blocked, failed, or cancelled.
- Grunt: resulting SHA/patch, changed files, check commands, working
  directories, tested revision, exit codes, redacted log references, blockers.
- Reviewer: reviewed base/head and diff identity, structured findings with
  severity, title, path/line range, rationale/evidence, and suggested action.
- Failure/cancellation details and bounded diagnostic artifacts.

Validate output against the negotiated contract. Distinguish a successful
review with zero findings from a timeout, missing output, or malformed output.
Never turn a worker's "done" message into an authoritative test or review pass.
The brain evaluates evidence; the backend reports it faithfully.

The precise API transport/schema remains to be selected using existing
app-server/thread abstractions. This document does not add tool or RPC names.

## Cross-provider team messaging

The future host messaging bridge negotiates team-mailbox capabilities and
preserves envelope IDs, correlation, and authenticated source metadata for
send/receive/ack across OpenAI-backed, MultiDex/Codex-native, and Claude-backed
sessions. MultiDex and Codex-native identify a harness/backend, not an LLM
vendor. Provider-neutral adapters expose the host bridge; Omnigent owns team
membership, ACLs, routing, and durable mailboxes. No agent receives messaging
credentials or assumes a shared host.

The conceptual envelope includes `protocol_version`, `message_id` and
`idempotency_key`, `team_id`, source session ID/role/display name,
`destination_session_id`, task or review-round ID, optional reviewed head SHA,
kind (`task`, `question`, `answer`, `progress`, `status_query`, `status_reply`,
`result`, or `cancel`),
`correlation_id`/`in_reply_to`, payload or artifact references, sequence, and
creation/expiry timestamps. The host stamps the authenticated source, validates
membership and permissions, and scopes payloads and artifact access to the
team. Display names are not authorization.

MultiDex's process-local mailbox is not cross-host durable transport. The host
bridge uses Omnigent's durable envelope, mailbox, and recovery contract;
MultiDex does not own Redis, a durable cross-provider transport, or a second
scheduler. Preserve scoped roles and native approval boundaries, including the
separation between child-dispatch and peer-message tools. Do not inherit a
child-tool approval exemption for peer sends. Unsupported mailbox, role, or
authorization capabilities fail clearly during negotiation, especially on
older backends. Omnigent's
[cross-provider messaging design](https://github.com/ModelCloud/omnigent/blob/design/brain-executor-reviewer-roles/docs/BRAIN_EXECUTOR_REVIEWER_DESIGN.md#cross-provider-team-messaging)
is authoritative for the team mailbox contract; its
[messaging-tool and approval boundary](https://github.com/ModelCloud/omnigent/blob/main/docs/session-messaging-tool-boundary.md)
documents the existing native approval scopes.

Messaging never grants planning, spawning, editing, or publication authority.
The host applies role and phase policy: clarification requests during
independent review route through the brain with only minimal source-grounded
facts; reviewers cannot see implementation transcripts or sibling findings
before all configured reviewers' independent results are sealed. Post-seal
discussion cannot rewrite those results, and every new revision receives fresh
review contexts.

Future acceptance checks cover cross-provider brain/grunt/brain round trips,
grunt peer question/reply, reviewer factual clarification and post-seal
exchange, reconnect replay and duplicate IDs, unavailable hosts, denied native
approval, wrong-team or spoofed sources, shared-source preflight, stale rounds,
and review isolation. These are design targets, not tests run by this proposal.

### Non-destructive brain/worker status ping/pong

Negotiate a status-control capability that lets the brain query a live grunt
or reviewer and receive its summary without replacing or ending the assignment.
`status_query` carries a query/correlation ID, team/task/attempt identity,
destination, deadline, and scope: `now`, `since_time` with a UTC timestamp, or
`since_commit` with a full repository commit SHA.

All brain/sub-agent messages use asynchronous send-and-callback semantics.
Return a durable acceptance receipt and message/query ID promptly; do not hold
the sender's tool invocation open until an agent reply arrives. The brain
continues other work without polling, and multiple outstanding queries may
receive replies out of order.

Preserve the email-like reply envelope: authenticated source session/agent ID,
role/display name, destination team/brain, message ID, `in_reply_to` and
correlation ID, task/attempt/revision, typed payload, timestamps, and status.
Omnigent retains the original question/reference and delivers replies or
timeout/refusal/cancellation events to the brain's inbox at a safe boundary,
waking it if idle. These are runtime callbacks, not executable functions or
concurrent turns against the same conversation. Forward and acknowledge by
message ID; never associate a reply with whichever question was asked last.
Acceptance, delivery, and a completed reply remain distinct states.

At a safe execution boundary, the worker reads the control request, summarizes
its activity, sends `status_reply` through the host mailbox, then continues
the same task/session/attempt/workspace. The reply includes current action and
phase, work completed within the scope, pending work/blockers, head SHA and
dirty-work status, check progress, artifact references, observation timestamp,
event watermark, and any coverage limits. It is not the task's terminal result
and must arrive while the task is still in progress when the worker can yield.

Do not implement ping as a fresh assignment, blind steering text, concurrent
model turn, or interruption that kills a running tool. MultiDex's adapter must
preserve the worker's execution position and role restrictions. If a tool is
busy, send separately identified runtime-observed status and leave the
worker-authored reply pending until a safe yield. Unsupported control injection,
disconnects, and deadlines return explicit capability/unavailable/timeout
states; never claim a stale heartbeat is a current worker-authored answer.

Use coarse task events/checkpoints and available code/test artifacts, not a
second full transcript or continuous reasoning snapshot. Resolve time windows
against event timestamps and sequence watermarks; validate commit anchors and
their relationship to observed HEAD, reporting dirty edits separately.
Missing, ambiguous, non-ancestor, or expired anchors yield an explicit partial
or unsupported answer instead of a substituted range.

Preserve query IDs, acknowledgement state, and attempt/revision correlation
through reconnects. Duplicate pings do not repeat task work, late pongs cannot
be attributed to replacement attempts, and status replies cannot resurrect
cancelled jobs. The host bounds frequency and size; no extra grunt slot is
allocated for a query. Reviewer pongs describe inspection progress/blockers,
not unsealed findings or sibling deliberation.

Future tests must observe a mid-task agent-authored reply followed by continued
execution in the same session, for grunts and reviewers, and cover scoped
queries, long-running commands, unsupported capabilities, deduplication,
disconnect/reconnect, invalid anchors, and stale attempts. The host/backend
contract is proposed here; no status channel is implemented by this PR.
Also verify that the brain progresses after send, that replies delivered in
reverse query order keep correct correlation, and that replayed callbacks after
reconnect do not duplicate work or require the brain to poll.

## Context lifecycle

The single main session is named `brain`. All execution sub-agents are named
`grunt-<uniquename>` and all review sub-agents `reviewer-<uniquename>`. The
runtime derives the role prefix and allocates a unique suffix within the parent
session; names remain visible in UI and reports. Task labels are separate
metadata, not name prefixes. Names never determine context reuse: grunt
continuation uses its task/session ID, and each fresh reviewer session gets a
new unique name and session ID on every round.

### Brain

Keep one normal persistent brain session. Omnigent resolves and pins the brain's
default provider/model/variant/speed/reasoning and compatible harness when
creating the BGR session; changing that default never mutates the active brain.
Do not inherit the brain's selection implicitly into every managed worker.
Grunt and reviewer profile changes are versioned and apply to future jobs/rounds;
in-flight jobs retain their resolved snapshots.

### Grunt

Create a task-scoped session/worktree at the brain-selected starting revision.
Continue the same task session for concrete fix instructions where appropriate.
Do not fork the entire brain transcript by default; supply the bounded task
contract and necessary evidence. Do not resume an unrelated task by display name.

### Reviewer

One or two review slots represent settings, not reusable conversations. Every
review job starts a new thread with no initial parent or implementation history,
no previous review transcript, and no sibling review results. Do not resume or
fork a reviewer thread for the next publication. Allocate a new unique
`reviewer-<uniquename>` display name and session ID for every round.

Use a frozen base/head and diff, plus surrounding source at the reviewed
revision. Branch names are display metadata, not revision identity. Reviewers
may receive the acceptance contract and selected trusted instructions, but not
the brain's deliberations or the implementer's explanation as a substitute for
independent inspection.

A retry/reconnect must distinguish reattaching to the same job from launching
a fresh attempt. Idempotency and attempt IDs prevent accidental continuation of
an old review under a new round.

## Runtime restrictions

Role capabilities must be enforced by tools, permission inheritance, and
sandbox/workspace controls; prompts alone are insufficient.

- Grunts cannot spawn descendants, expand role permissions, push remote
  branches, create PRs, or approve their own output. Local commits may be used
  to return a precise result revision. Publication credentials are absent.
- Reviewers cannot mutate reviewed source, spawn descendants, or access
  publication credentials. Disable or constrain every write-capable path,
  including shell commands, not only editing tools.
- If review requires running tests, use a disposable unprivileged environment
  with controlled scratch writes, isolated from the immutable source snapshot.
  Never run PR code with privileged credentials.
- Brain-authorized publication is a host operation subject to existing user
  approval and branch protection, not a privilege workers inherit.
- Resume, tool aliases, configuration overrides, and provider switches cannot
  weaken the role ceiling. Unsupported enforcement fails capability preflight.

Keep role policy separate from provider transport. No role is tied to Claude,
OpenAI, or a specific harness. A gateway is not a model vendor; the host checks
reviewer diversity from resolved catalog metadata. Explicit model/provider
choices must not silently fall back to a different selection.

## Completion, cancellation, and delivery

Emit structured completion tied to the exact job/attempt and reviewed/tested
revision. Preserve enough identity to reconcile reconnects and reject mismatched
results; Omnigent is the durable source of truth for tasks and review rounds.
This is at-least-once transport with idempotent logical handling, not an
exactly-once distributed execution promise.

Cancellation stops the worker process tree where supported and returns a
cancelled result without a fabricated success. Late results retain their
original revision identity; Omnigent determines whether they are superseded.
Jobs must not continue unauthorized publication after the parent disappears.

Omnigent's trusted publisher delivers the same results to a PR discussion note
and the brain inbox. MultiDex reviewers have no direct GitHub credentials and
do not post comments themselves. The brain deduplicates/adjudicates findings
and sends only concrete accepted fixes to grunts.

## Relationship to publication

Default review begins after a brain-owned publication checkpoint has successfully
pushed and verified the remote head. MultiDex shell text matching and local
git hooks are not authoritative review triggers. Local commits followed by one
push form one checkpoint rather than several review rounds.

Omnigent freezes base/head, acceptance-contract version, reviewer configuration,
and rerun generation; starts fresh reviewers; persists results; and handles
deadlines, retries, stale rounds, and partial failure. A newer publication never
inherits approval from an older review. Configured reviewers must all finish
successfully before the host labels that round complete.

## Resumption contract

BGR must tolerate an individual brain/grunt/reviewer failure and a whole-team
or server outage without persisting every agent's internal state. Omnigent owns
a compact durable recovery ledger plus artifact storage; model context and
MultiDex process-local state are not authoritative copies. The companion
design defines the ledger and coordinator recovery protocol.

The backend contributes coarse checkpoints, not token-by-token snapshots:

- Team/task/round identity, logical agent ID, role/display name, backend
  session/thread ID, host, attempt ID, and current coordinator fencing epoch.
- Accepted contract and resolved profile revision, launch acknowledgement,
  coarse phase, last checkpoint and heartbeat, blocker, and terminal status.
- Exact repository/base/result revisions, workspace identity, durable
  patch/checkpoint references and checksums, and tested-revision evidence.
- Message IDs/acknowledgement cursors and resumability information: whether a
  live session can reattach, a native session can resume, or only a fresh
  attempt from durable artifacts is possible.

Expose status/reconnect/checkpoint/cancel capabilities through the negotiated
host contract, using existing thread and persistence abstractions. A backend
resume handle is access-controlled metadata, not an API key in a job payload.
Preserve upstream transcript/context construction; do not introduce a second
transcript format or claim that providers share resumable conversations.

### Reattach versus reconstruct

If the same attempt is still alive, reconnect to it only after checking its
identity, role ceiling, profile, and workspace. A transport timeout is not
proof of process death. Report an existing persisted completion instead of
starting duplicate work after a lost acknowledgement.

When native session context is available and valid, the host may resume that
same attempt. When it is unavailable, report that explicitly: Omnigent creates
a fresh attempt using the original contract, latest verified code artifact,
completed evidence, and pending instructions. Missing uncheckpointed edits
may need to be redone; never imply that a fresh session recovers lost reasoning.
The logical task persists, but replacement attempt/session IDs are new.

A grunt checkpoints only at safe milestones, such as a recoverable patch or
local commit and completed checks. Recovery artifacts go to host-managed
durable storage; a local commit on a lost machine is not durable protection.
This does not grant grunts remote push or PR permissions. Checks are reusable
only when their revision and relevant environment match; arbitrary commands
with uncertain side effects must not be automatically replayed.

A reviewer may reattach to its same live attempt. If replacement is needed,
create a new reviewer session/name for the frozen revision and contract, with
no partial transcript or sibling findings. The host preserves other sealed
slots, retries only the failed slot, and creates a new round if head or review
configuration changed. Missing reviewers never count as approval.

### Fencing and collective recovery

The host supplies a coordinator epoch and attempt lease. Reject stale dispatch,
resume, and result-advancement requests at the backend boundary. Lease expiry
prevents new work; an already-running local command may still finish. Before a
replacement writes, confirm the prior process tree is stopped or safely
isolated; never run two attempts in the same writable workspace. If the backend
cannot fence/isolate a disconnected attempt, report a blocker rather than
silently starting another one.

When the brain disappears, grunts may checkpoint/finish only already authorized
bounded work while leases permit; they do not promote themselves to brain.
Results remain available for the recovered parent. Omnigent restores one active
logical `brain`, reconstructs its task map and pending decisions from the
ledger when native context is unavailable, and reconciles parent/child routing
without weakening ownership or native approvals. MultiDex reports process and
artifact reality; it does not elect a second coordinator.

On collective restart, await host reconciliation before accepting new jobs.
Preserve cancellation and terminal state, retain stale-attempt results for
diagnosis without applying them, and report unavailable artifacts explicitly.
Omnigent reconciles remote pushes and PR-note markers before retrying uncertain
publication; workers never infer successful publication from local state.

Future tests must distinguish reconnect from confirmed process loss; cover
lost launch/completion acknowledgements, missing native context or worktrees,
partial checkpoints, stale epochs, two recovery coordinators, one lost review
slot, and whole-team restart. Assert preserved role restrictions and task IDs,
new replacement attempt IDs, no duplicate writers, no silent loss or false
success, and fresh-context replacement review. These are future tests, not
checks completed by the first backend slice.

## Implementation sequence and future verification

1. Agree the versioned host/backend contract and capability handshake.
2. Wire explicit selections and role restrictions through existing thread and
   child configuration paths without changing ordinary session behavior.
3. Add fresh reviewer execution, structured results, safe checkpoint/resume,
   and cancellation/reconnect correlation using existing review/mailbox primitives.
4. Integrate with the BGR selection in the Omnigent server/host UI.

Acceptance tests for future implementation, not validation performed by this PR:

- A BGR job carries the exact resolved provider/model/variant/speed/reasoning,
  harness compatibility, and configuration revision; unsupported explicit
  combinations fail without remapping.
- Grunt default selection is used unless the brain explicitly assigns a
  permitted profile within user policy; no implicit auto-routing or required
  static worker pool.
- Omnigent starts only needed grunts up to the persisted dynamic cap across
  turns/restarts; excess work queues or is rejected, and reviewers do not use
  the cap. Lowering the cap does not terminate active grunts and blocks new
  starts until below the new cap. MultiDex does not schedule a second pool.
- Brain defaults resolve at session creation; profile revisions affect future
  jobs/rounds without silently changing an active brain.
- Grunt continuation retains only its task context. Each review attempt
  begins without prior conversation and uses a newly allocated unique reviewer
  name and session ID.
- A reviewer reads the exact frozen revision and surrounding source, cannot
  edit it or obtain publication credentials, and cannot enable delegation.
- Role ceilings survive resume/configuration changes and cover shell/tool paths.
- Structured results preserve SHA, command, exit code, and finding locations;
  malformed/empty/failed output is not confused with success.
- Duplicate delivery/reconnect and cancellation do not produce new logical
  review jobs or authorize stale results.
- BGR offers Polly as a separate existing choice and does not change its
  behavior; BGR selection exposes one or two reviewers and independent
  provider/model/variant/speed/reasoning settings per active reviewer.
- An integration cycle has two different reviewers report the same SHA to the
  host, followed by a brain-assigned fix and entirely fresh review sessions.
- Existing non-managed threads, upstream conversation formatting, provider
  routing, continuation, and compaction behavior remain unchanged.

## Out of scope

Beyond the first backend slice: no release packages, dependency changes, model
defaults, live automations, new credential storage, automatic merging, or
standalone MultiDex orchestration. The implemented RPC names and limits above
supersede conceptual names elsewhere in this design.
