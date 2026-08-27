# Responsiveness

## Ownership

`kvim-runtime` owns reusable bounded scheduling, cancellation, deadlines, and
result delivery. `kvim-embed` owns facade-facing readiness, completion,
application, and shutdown values for one high-level editor. `EditorDriver` is a
transitional internal owner for the `kvim-tui` compatibility path. The host
event loop owns that instance's visible editor state. The standalone `kvim`
binary owns the terminal event loop.

## Event Loop

No host or terminal event loop may run:

- filesystem reads, writes, or directory scans,
- external processes, including ripgrep, a language server, and a formatter,
- Git commands,
- Language Server Protocol (LSP) requests or responses,
- Tree-sitter parsing or highlighting,
- formatting,
- clipboard reads or writes that can block,
- any other blocking or unbounded work.

An event loop resolves input, applies pure state transitions, applies completed
typed results, and renders after a visible state change. An embedded host supplies
resolved input. The standalone binary also normalizes terminal events.

Do not use an unconditional frame loop. A terminal event, a resize, an expired
deadline, or an accepted background result requests a render.

### Time-Driven Transitions

The elapsed time alone drives two state changes: the which-key overlay and the
notification overlay. The session reports the earlier elapsed time of the two,
and the loop wakes for it.

The loop passes that same elapsed time into every entry point of the session,
so the session reads no clock. The editor log stamps every entry with it, and
the log drives no transition of its own. [`windows.md`](windows.md) owns the
log.

The notification overlay advances its spinner one frame for each frame
interval, and it removes a finished item after its lifetime.
[`language-services.md`](language-services.md) owns that overlay. It runs no
frame loop: it reports a time only while it holds a running item or a finished
item, and every transition that it performs leaves a strictly later time
behind.

Two rules keep that path safe:

- Report a time only when a transition can consume it. A reported time that no
  transition clears would keep the loop out of its wait forever, and the editor
  would stop serving input. The overlay therefore reports no time while the
  pending sequence holds no key, because the rows list the keys that follow a
  sequence and a pending count alone shows no overlay.
- Run at most one catch-up transition for each reported time. The loop records
  the time that it already served. A transition that leaves the same time behind
  never runs twice, so the loop always returns to waiting for an event. The rule
  bounds the failure, whatever produced the unclearable time.

Every submission loop of the event loop is bounded by a named constant, so a
queue that offers the same work again can never hold the loop.

### Request Dispatch

One iteration hands every queued request to the service that runs it before the
loop waits again. One submission can queue the work of another owner: a
formatting request that no language server accepts completes the save that
waited for it. The dispatch therefore repeats inside a named bound. A request
that stayed in its outbox would reach its service only after the next terminal
event, and the user would see the result of a command follow an unrelated key.

A refused submission is a state change like any other. It names its state on the
message line, so the dispatch reports its own redraw request and the frame
follows it. Every transition that changes a painted value must report that
change, including a transition that runs outside a terminal event. A dropped
redraw request leaves the changed message, marker, or overlay off the screen
until an unrelated event paints the next frame.

## Bounded Work

Run external commands through a bounded and cancellable process spawner. Run
processor-bound work through a bounded worker spawner. The host supplies these
spawners to an embedded driver and chooses isolated capacity or an explicitly
shared pool. Bound queues, process concurrency, worker concurrency, input sizes,
output sizes, caches, retries, and deadlines.

The library creates no asynchronous runtime and starts no detached task. The
host runs every returned driver future and supervises every submitted task.
Synchronous syntax highlighting is bounded processor work. Submit it through the
worker spawner. Never call it directly from an event loop.

Submission never waits for capacity. When no permit or result slot is available,
submission returns a typed saturated result immediately. The event loop then
keeps the previous visible state and reports the saturated state. This keeps the
event loop free from backpressure waits.

Each service owns its permits until the work and the result delivery finish.
Creating another client of a service does not create more capacity.

The worker spawner accepts two kinds of job. An optional job changes no durable
state, so a cancellation or deadline drops it and the caller keeps its previous
visible state. A committing job can change durable state. A deadline can stop
it only before commit begins. Once commit begins, it masks cancellation and
owns its result reservation until it reports the actual outcome. Shutdown
tracks the job until that publication. See [Mandatory Event
Delivery](#mandatory-event-delivery).

### Runtime Bounds

The `runtime` module names each bound as one constant. The constant and the row
below must always agree.

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Result queue default | `EVENT_QUEUE_CAPACITY` | 256 results | One editor keystroke starts few requests, so 256 results absorb a burst without hiding a stalled event loop. |
| Result queue maximum | `EVENT_QUEUE_CAPACITY_MAX` | 4,096 results | A supplied runtime can absorb a larger host burst without retaining an unbounded result queue. |
| Process concurrency | `PROCESS_CONCURRENCY_LIMIT` | 8 processes | The editor runs few external commands together: one search, one formatter, and one clipboard command. Eight leaves headroom and still bounds the child count. |
| Worker concurrency | `WORKER_CONCURRENCY_LIMIT_MAX` | 1 to 8 jobs | The runtime clamps the detected parallelism into this range, so a large host does not start dozens of parser threads for one editor. |
| Process input | `PROCESS_INPUT_BYTES_MAX` | 8 MiB | A formatter receives one buffer. [`text-model.md`](text-model.md) bounds one file at 4 MiB, so 8 MiB keeps headroom for expansion. |
| Process output default | `PROCESS_OUTPUT_BYTES_DEFAULT` | 1 MiB | A picker or clipboard result stays small. The default fails early for an unexpected flood, and a caller with a larger answer names its own limit. |
| Process output maximum | `PROCESS_OUTPUT_BYTES_MAX` | 16 MiB | No caller may raise the limit above this value. The editor never needs more than four times the largest loaded file. |
| Process deadline default | `PROCESS_DEADLINE_DEFAULT` | 10 s | A cold formatter or a large search needs seconds. Ten seconds reports a stuck command before the user waits longer. |
| Worker deadline default | `WORKER_DEADLINE_DEFAULT` | 5 s | A parse or a highlight of a bounded file finishes far below this value. Five seconds reports a runaway job. |

kvim uses a smaller process-output maximum than ReviewGraph, which allows
129 MiB for large Git output. kvim edits bounded files and never captures a
repository-sized result.

A caller may set its own output limit and its own deadline for one request,
inside the maximum values above. The external formatter of
[`language-services.md`](language-services.md) names a larger output limit than
the default, because it reads back a complete document. The process output limit
counts standard output and standard error together, so a noisy standard error
cannot double the captured bytes.

[`files.md`](files.md) owns picker, file, tree, and workspace-watch limits.
[`language-services.md`](language-services.md) owns analysis and protocol
limits.

The workspace watcher is the one background service that produces a stream
instead of a result for one request. It needs no publication gate, because no
burst replaces another one: every burst names its own change. It owns the same
duties as every other service. Its platform callback and its coalescing task run
beside the event loop, its queues are bounded, and a full queue drops events and
reports the drop instead of growing.

The watcher performs one filesystem read of its own, for its registration. It
walks the workspace once and adds one watch for each directory that it keeps.
The walk skips every generated directory name, so it reads no build output
directory and no repository database. The walk is bounded by directories, by
depth, and by the entries of one directory. [`files.md`](files.md) owns that
rule and its bounds.

That walk runs after the first frame. The start of the watcher places no watch
and reads no directory. It hands the root to the coalescing task, and that task
performs the walk on a blocking thread. The event loop draws its first frame
while the walk runs, so no workspace delays that frame.

No watch covers the window between the first frame and the completed
registration. The coalescing task therefore publishes one burst of `Dropped` as
it opens its stream, which asks the sidebar to read every expanded directory
again. A change inside the window reaches the editor through that read, so the
deferred registration loses no change. [`files.md`](files.md) owns the rule.

That opening burst also carries the coverage of the registration. A registration
that covers a part of the workspace therefore reports itself with the first
published value, and the editor shows that report on the next frame. The report
needs no second mechanism and no second frame. Every later batch reports its own
coverage with the burst of the same window. [`files.md`](files.md) owns the
rule and the once-for-each-session limit of that report.

The watcher performs one further read for each burst. A watch covers one
directory alone, so a directory that appeared after the last walk needs its own
watch. The coalescing task reads every directory that the burst names, walks
each new subtree, and adds the new watches in one batch. It performs that work
beside the event loop, on a blocking thread, and the event loop performs no
watch call. The read obeys the same bounds as the walk at start, and the task
skips a directory that already carries a watch, so one burst costs bounded time.

## Request Identity And Publication

Each request carries editor instance identity, buffer identity where applicable,
generation and version for text-derived work, a cancellation owner, and an
explicit deadline. LSP requests also carry project and server identity. A newer
request for the same slot makes an older request obsolete. Instance identity is
validated before result application in every build profile. A wrong-instance
result returns a typed rejection and cannot mutate state, advance a clock, or
release another editor reservation.

A publication gate stores only the newest request identity for each slot. The
event loop checks the gate before it applies a result. The gate does not mutate
visible state. The gate rejects an obsolete picker result, preview result,
completion result, analysis result, formatting result, or language-server
result.

Obsolete work may finish its cleanup. Its result must not change visible state
and must not populate a cache for a newer request.

Build a fallible state change outside live state. Validate the complete
candidate. Apply it on the event loop as one transition. Cancellation, timeout,
worker failure, saturation, or invalid output leaves the previous valid state
usable.

## Mandatory Event Delivery

Optional results can coalesce or return a typed saturated outcome. A mandatory
event after a durable side effect must not be lost.

An editor driver reserves bounded outbox capacity before it accepts a save,
workspace mutation, or review-comment submission. Saturation refuses the
operation before it starts. Accepted work follows `Reserved -> Running ->
Committed -> Published`.

Cancellation can stop work before commit. A deadline can stop work only before
commit begins. Once commit starts, the operation owns its reservation until it
publishes its actual `Committed`, `Unchanged`, or `Indeterminate` outcome. It
must not report timeout, cancellation, or shutdown completion while durable
state can still change. Failure before commit releases the reservation.

A successful write, workspace mutation, or review-comment submission publishes
its typed event. An indeterminate filesystem outcome reserves mandatory delivery
and schedules bounded reconciliation before visible state claims agreement with
disk.

`RedrawRequested` uses one coalesced latch. A full component event queue returns
a typed `Saturated` outcome. It never silently drops the oldest or newest event.
[`embedding.md`](embedding.md) owns the complete event lifecycle.

### Recorded Outcomes

A background job that changes no visible state reports nothing on the message
line. A user then reads no cause for a file that lost its highlighting, or for
a path completion that stayed empty. The editor therefore records a selected
set of these outcomes in the editor log. [`windows.md`](windows.md) owns the
log, the entry shape, and the rule that collapses a repeated report.

The editor records these outcomes:

| Job | Outcome | Severity |
|---|---|---|
| `analysis` | A newer buffer version displaced the result. | `Info` |
| `analysis` | The worker service accepted no job. | `Warning` |
| `analysis` | The job was cancelled, passed its deadline, or failed. | `Info` or `Warning` |
| `analysis` | The adapter passed a bound, or returned no usable result. | `Warning` |
| `walk` | The walk was cancelled, passed its deadline, or failed. | `Info` or `Warning` |
| `formatter` | A newer buffer version displaced the answer. | `Info` |

A cancelled job carries the `Info` severity, because a newer request in the
same slot cancels the older one and that is a normal state. Every other outcome
carries `Warning`.

The editor records the obsolete result of one slot alone, and that slot is the
analysis slot. The log compares one report with its newest entry alone, so two
obsolete kinds that alternate cost two entries for each keystroke. That pair
would fill the log and remove every earlier report. The picker, the preview,
and the path completion also reject an obsolete result, and the editor records
none of the three.

The editor records no outcome that already reaches the message line. The
formatter that the host does not hold, the formatter that refused a document,
and the missing `git` command all reach it, so the `MESSAGE` entry beside them
already holds the report. The editor also records no failed Git status read,
because that failure names a directory outside a repository and a timeout with
one value, and such an entry would name no outcome that a reader can act on.

## Latency Budgets

- Process one terminal event and its pure state transition within 8 ms at p95.
- Build and render one terminal frame within 16 ms at p95.
- Never stall the event loop above 50 ms.
- Never wait for worker capacity or result-queue capacity on the event loop.
- Publish cancellation promptly. Check cancellation before and after blocking
  work.

A background operation that cannot meet its deadline returns a typed timeout
result. The previous visible state stays usable.

## Shutdown

The standalone binary shuts down through the driver of its own editor, so both
paths run the same order:

1. Stop the workspace watcher, so it queues no further directory read.
2. Reject new work.
3. Cancel all owned work.
4. Wait for accepted tasks to finish cleanup.

The watcher, the language services, and the runtime each end through one
consuming operation, so no caller can submit after it. The coalescing task of
the watcher owns the platform watcher, so the shutdown cancels that task and
waits for it. The task drops the platform watcher as it ends, which ends the
platform callback thread. The shutdown therefore returns only after no further
event can reach any queue.

A shutdown during the registration waits for that registration. The blocking
thread holds the platform watcher until it returns, so the task waits for that
thread and then drops the watcher. The registration is bounded, so the wait is
bounded as well.

Dropping a process future kills its child process. Dropping the runtime remains
a best-effort safety net. Normal editor shutdown must use the explicit consuming
shutdown operation.

Embedded shutdown consumes one `EditorDriver`. It rejects new work, cancels
pre-commit work, closes its optional services, and waits only until the supplied
deadline. It does not abort a task that can have committed a side effect. If the
deadline expires first, shutdown returns a bounded, must-use `ShutdownDrain`.
The drain owns remaining tasks, event reservations, and mandatory delivery. The
host keeps its runtime alive until the drain completes.

Terminal restoration runs after runtime shutdown and also runs while the process
unwinds from a panic. See [`architecture.md`](architecture.md) for the release
profile that keeps unwinding available.

### Termination Signals

The default action of `SIGTERM`, `SIGINT`, and `SIGHUP` ends the process while
the editor still holds raw mode, the alternate screen, and the enhanced keyboard
flags. No restore step runs, so a terminated editor would leave the terminal
unusable.

The terminal crate owns a termination source. It reports the first of those
three signals as one value. The event loop reads that value beside its terminal
events, its worker results, and its language events, in every wait it performs.
The loop must observe a termination in each of its waits, including the wait
that holds a deadline.

A termination request ends the event loop through the same outcome as the last
closed window. The shutdown order above then runs, and terminal restoration runs
after it. The signal path adds no second exit path and no second restore step
list.

The source reports one request and then reports nothing again. A second signal
therefore needs no handling: shutdown after the first request is bounded by the
worker deadlines, the process deadlines, and the language server shutdown
deadline.

Only Unix defines these signals. macOS and Linux are the supported platforms. On
another platform the source reports no request, and the remaining exit paths
stay unchanged.
