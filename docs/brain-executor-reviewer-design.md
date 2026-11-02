# BGR: role-constrained brain, grunt, and reviewer execution

Status: **design proposal only**. No Rust behavior, configuration schema,
protocol, prompts, permissions, or binaries are changed by this document.
Names below are conceptual contracts, not supported configuration/API fields.

Companion: [Omnigent orchestration design](https://github.com/ModelCloud/omnigent/blob/design/brain-executor-reviewer-roles/docs/BRAIN_EXECUTOR_REVIEWER_DESIGN.md).

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
kind (`task`, `question`, `answer`, `progress`, `result`, or `cancel`),
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
before both independent results are sealed. Post-seal discussion cannot rewrite
those results, and every new revision receives fresh review contexts.

Future acceptance checks cover cross-provider brain/grunt/brain round trips,
grunt peer question/reply, reviewer factual clarification and post-seal
exchange, reconnect replay and duplicate IDs, unavailable hosts, denied native
approval, wrong-team or spoofed sources, shared-source preflight, stale rounds,
and review isolation. These are design targets, not tests run by this proposal.

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

## Implementation sequence and future verification

1. Agree the versioned host/backend contract and capability handshake.
2. Wire explicit selections and role restrictions through existing thread and
   child configuration paths without changing ordinary session behavior.
3. Add fresh reviewer execution, structured results, and cancellation/reconnect
   correlation using existing review/mailbox primitives.
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

No implementation in this PR; no release packages, dependency changes, model
defaults, live automations, new credential storage, automatic merging, or
standalone MultiDex orchestration. Exact schema names and capability encoding
remain implementation-review decisions shared with the Omnigent companion.
