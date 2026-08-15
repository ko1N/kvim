# Responsiveness

## Ownership

The `runtime` module owns background scheduling, cancellation, deadlines,
request identity, and result delivery. The `tui` module owns the terminal event
loop and visible editor state.

## Event Loop

The terminal event loop must not run:

- filesystem reads, writes, or directory scans,
- external processes, including ripgrep and `rust-analyzer`,
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

The concrete queue sizes, concurrency limits, output limits, and deadlines are
not yet decided. Slice 2 must record them here before implementation depends on
them. [`files.md`](files.md) owns picker and file limits.
[`language-services.md`](language-services.md) owns analysis and protocol
limits.

## Request Identity And Publication

Every background request has an explicit identity. A newer request for the same
slot makes an older request obsolete. Every request has one cancellation owner.
Every request has an explicit deadline.

A request that reads or transforms buffer text also carries the buffer version
that produced its input. See [`text-model.md`](text-model.md).

A publication gate stores only the newest request identity for each slot. The
event loop checks the gate before it applies a result. The gate does not mutate
visible state. The gate rejects an obsolete picker result, preview result,
analysis result, formatting result, or language-server result.

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

1. Reject new work.
2. Cancel all owned work.
3. Wait for accepted tasks to finish cleanup.

Dropping a process future kills its child process. Dropping the runtime remains
a best-effort safety net. Normal editor shutdown must use the explicit consuming
shutdown operation.

Terminal restoration runs after runtime shutdown and also runs while the process
unwinds from a panic. See [`architecture.md`](architecture.md) for the release
profile that keeps unwinding available.
