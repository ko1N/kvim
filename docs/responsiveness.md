# Responsiveness

## Ownership

The `runtime` module owns background scheduling, cancellation, deadlines,
request identity, and result delivery. The `tui` module owns the terminal event
loop and visible editor state.

## Event Loop

The terminal event loop must not run:

- filesystem reads, writes, or directory scans,
- external processes, including ripgrep, a language server, and a formatter,
- Git commands,
- Language Server Protocol (LSP) requests or responses,
- Tree-sitter parsing or highlighting,
- formatting,
- clipboard reads or writes that can block,
- any other blocking or unbounded work.

The event loop processes normalized terminal events, applies pure state
transitions, applies completed typed results, and renders after a visible state
change.

Do not use an unconditional frame loop. A terminal event, a resize, an expired
deadline, or an accepted background result requests a render.

### Time-Driven Transitions

The elapsed time alone drives two state changes: the which-key overlay and the
notification overlay. The session reports the earlier elapsed time of the two,
and the loop wakes for it.

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

Run external commands through a bounded and cancellable process service. Run
processor-bound work through a bounded worker service. Bound queues, process
concurrency, worker concurrency, input sizes, output sizes, caches, retries, and
deadlines.

Submission never waits for capacity. When no permit or result slot is available,
submission returns a typed saturated result immediately. The event loop then
keeps the previous visible state and reports the saturated state. This keeps the
event loop free from backpressure waits.

Each service owns its permits until the work and the result delivery finish.
Creating another client of a service does not create more capacity.

### Runtime Bounds

The `runtime` module names each bound as one constant. The constant and the row
below must always agree.

| Bound | Constant | Value | Rationale |
|---|---|---|---|
| Result queue capacity | `EVENT_QUEUE_CAPACITY` | 256 results | One editor keystroke starts few requests, so 256 results absorb a burst without hiding a stalled event loop. |
| Process concurrency | `PROCESS_CONCURRENCY_LIMIT` | 8 processes | The editor runs few external commands together: one search, one formatter, and one clipboard command. Eight leaves headroom and still bounds the child count. |
| Worker concurrency | `WORKER_CONCURRENCY_LIMIT_MAX` | 1 to 8 jobs | The runtime clamps the detected parallelism into this range, so a large host does not start dozens of parser threads for one editor. |
| Process input | `PROCESS_INPUT_BYTES_MAX` | 8 MiB | A formatter receives one buffer. [`text-model.md`](text-model.md) bounds one file at 4 MiB, so 8 MiB keeps headroom for expansion. |
| Process output default | `PROCESS_OUTPUT_BYTES_DEFAULT` | 1 MiB | A picker or clipboard result stays small. The default fails early for an unexpected flood, and a caller with a larger answer names its own limit. |
| Process output maximum | `PROCESS_OUTPUT_BYTES_MAX` | 16 MiB | No caller may raise the limit above this value. The editor never needs more than four times the largest loaded file. |
| Process deadline default | `PROCESS_DEADLINE_DEFAULT` | 10 s | A cold formatter or a large search needs seconds. Ten seconds reports a stuck command before the user waits longer. |
| Worker deadline default | `WORKER_DEADLINE_DEFAULT` | 5 s | A parse or a highlight of a bounded file finishes far below this value. Five seconds reports a runaway job. |

Kvim uses a smaller process-output maximum than ReviewGraph, which allows
129 MiB for large Git output. Kvim edits bounded files and never captures a
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

The watcher performs one filesystem read of its own, when it starts. It walks
the workspace once and adds one watch for each directory that it keeps. The walk
skips every generated directory name, so it reads no build output directory and
no repository database. The walk is bounded by directories, by depth, and by the
entries of one directory, so a very large workspace costs bounded time before
the first frame. [`files.md`](files.md) owns that rule and its bounds.

## Request Identity And Publication

Every background request has an explicit identity. A newer request for the same
slot makes an older request obsolete. Every request has one cancellation owner.
Every request has an explicit deadline.

A request that reads or transforms buffer text also carries the buffer version
that produced its input. See [`text-model.md`](text-model.md).

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

Shutdown runs in this order:

1. Stop the workspace watcher, so it queues no further directory read.
2. Reject new work.
3. Cancel all owned work.
4. Wait for accepted tasks to finish cleanup.

The watcher, the language services, and the runtime each end through one
consuming operation, so no caller can submit after it. The watcher drops its
platform watcher first, which ends the platform callback thread, and then waits
for its coalescing task.

Dropping a process future kills its child process. Dropping the runtime remains
a best-effort safety net. Normal editor shutdown must use the explicit consuming
shutdown operation.

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
