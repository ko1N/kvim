//! Behavior tests for the bounded runtime.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{
    EVENT_QUEUE_CAPACITY, EVENT_QUEUE_CAPACITY_MAX, PROCESS_INPUT_BYTES_MAX,
    PROCESS_OUTPUT_BYTES_MAX, ProcessRequest, PublicationGate, RequestSlot, Runtime, RuntimeError,
    RuntimeLimits, SaturatedResource, SubmitError, WORKER_CONCURRENCY_LIMIT_MAX, WorkKind,
};

/// Blocks the worker thread until the request is cancelled.
fn until_cancelled(cancellation: CancellationToken) {
    while !cancellation.is_cancelled() {
        std::thread::yield_now();
    }
}

fn shell(script: &str) -> ProcessRequest {
    let mut process = ProcessRequest::new("sh");
    process.args = vec!["-c".into(), script.into()];
    process
}

#[test]
fn default_limits_clamp_the_detected_parallelism() {
    let limits = RuntimeLimits::default();

    assert_eq!(limits.event_queue(), EVENT_QUEUE_CAPACITY);
    assert!(limits.workers() >= 1);
    assert!(limits.workers() <= WORKER_CONCURRENCY_LIMIT_MAX);
}

#[test]
fn runtime_limits_reject_capacities_outside_published_bounds() {
    assert!(matches!(
        RuntimeLimits::new(0, 1, 1),
        Err(SubmitError::InvalidLimits)
    ));
    assert!(matches!(
        RuntimeLimits::new(EVENT_QUEUE_CAPACITY_MAX + 1, 1, 1),
        Err(SubmitError::InvalidLimits)
    ));
    assert!(matches!(
        RuntimeLimits::new(1, WORKER_CONCURRENCY_LIMIT_MAX + 1, 1),
        Err(SubmitError::InvalidLimits)
    ));
    assert!(matches!(
        RuntimeLimits::new(1, 1, 9),
        Err(SubmitError::InvalidLimits)
    ));
}

#[test]
fn default_runtimes_share_the_one_process_pool() {
    let (first, _first_events) = Runtime::<()>::new();
    let (second, _second_events) = Runtime::<()>::new();

    assert!(Arc::ptr_eq(&first.process_permits, &second.process_permits));
}

#[test]
fn the_gate_keeps_only_the_newest_request_of_each_slot() {
    let root = CancellationToken::new();
    let gate = PublicationGate::default();
    let stale = gate.begin(RequestSlot::new(1), &root);
    let other = gate.begin(RequestSlot::new(2), &root);
    let newest = gate.begin(RequestSlot::new(1), &root);

    assert!(stale.cancellation().is_cancelled());
    assert!(!gate.accepts(&stale));
    assert!(gate.accepts(&newest));
    assert!(gate.accepts(&other));
    assert!(newest.id().get() > stale.id().get());

    gate.cancel_all();
    assert!(newest.cancellation().is_cancelled());
    assert!(other.cancellation().is_cancelled());
}

#[test]
fn process_bounds_are_checked_before_the_runtime_spawns() {
    let (runtime, _events) = Runtime::<()>::with_limits(RuntimeLimits::new(4, 1, 1).unwrap());
    let gate = PublicationGate::default();
    let root = runtime.cancellation_root();

    let mut oversized_output = shell("true");
    oversized_output.output_bytes_max = PROCESS_OUTPUT_BYTES_MAX + 1;
    assert!(matches!(
        runtime.submit_process(
            gate.begin(RequestSlot::new(1), &root),
            oversized_output,
            |_| ()
        ),
        Err(SubmitError::ProcessBounds)
    ));

    let mut empty_output = shell("true");
    empty_output.output_bytes_max = 0;
    assert!(matches!(
        runtime.submit_process(gate.begin(RequestSlot::new(2), &root), empty_output, |_| ()),
        Err(SubmitError::ProcessBounds)
    ));

    let mut oversized_input = shell("true");
    oversized_input.stdin = vec![0_u8; PROCESS_INPUT_BYTES_MAX + 1];
    assert!(matches!(
        runtime.submit_process(
            gate.begin(RequestSlot::new(3), &root),
            oversized_input,
            |_| ()
        ),
        Err(SubmitError::ProcessBounds)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submission_reserves_the_result_slot_before_the_worker_permit() {
    // One free result slot and one free worker permit. The accepted request
    // exhausts both. The next submission must name the result queue, because
    // the runtime reserves delivery first.
    let (runtime, _events) = Runtime::<()>::with_limits(RuntimeLimits::new(1, 1, 1).unwrap());
    let gate = PublicationGate::default();
    let root = runtime.cancellation_root();

    runtime
        .submit_worker(
            gate.begin(RequestSlot::new(1), &root),
            Duration::from_secs(10),
            until_cancelled,
        )
        .unwrap();

    assert!(matches!(
        runtime.submit_worker(
            gate.begin(RequestSlot::new(2), &root),
            Duration::from_secs(10),
            |_| (),
        ),
        Err(SubmitError::Saturated(SaturatedResource::EventQueue))
    ));

    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_saturation_is_reported_while_result_capacity_remains() {
    let (runtime, mut events) = Runtime::<()>::with_limits(RuntimeLimits::new(8, 1, 1).unwrap());
    let gate = PublicationGate::default();
    let root = runtime.cancellation_root();

    runtime
        .submit_worker(
            gate.begin(RequestSlot::new(1), &root),
            Duration::from_secs(10),
            until_cancelled,
        )
        .unwrap();

    assert!(matches!(
        runtime.submit_worker(
            gate.begin(RequestSlot::new(2), &root),
            Duration::from_secs(10),
            |_| (),
        ),
        Err(SubmitError::Saturated(SaturatedResource::Workers))
    ));

    root.cancel();
    assert!(matches!(
        events.recv().await.unwrap().result,
        Err(RuntimeError::Cancelled)
    ));
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_saturation_is_reported_at_submission() {
    let (runtime, mut events) = Runtime::<()>::with_limits(RuntimeLimits::new(8, 2, 1).unwrap());
    let gate = PublicationGate::default();
    let root = runtime.cancellation_root();

    runtime
        .submit_process(
            gate.begin(RequestSlot::new(1), &root),
            shell("sleep 10"),
            |_| (),
        )
        .unwrap();

    assert!(matches!(
        runtime.submit_process(
            gate.begin(RequestSlot::new(2), &root),
            shell("sleep 10"),
            |_| (),
        ),
        Err(SubmitError::Saturated(SaturatedResource::Processes))
    ));

    root.cancel();
    assert!(matches!(
        events.recv().await.unwrap().result,
        Err(RuntimeError::Cancelled)
    ));
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submission_after_the_event_loop_ends_is_rejected() {
    let (runtime, events) = Runtime::<()>::with_limits(RuntimeLimits::new(4, 1, 1).unwrap());
    let gate = PublicationGate::default();
    let root = runtime.cancellation_root();
    drop(events);

    assert!(matches!(
        runtime.submit_worker(
            gate.begin(RequestSlot::new(1), &root),
            Duration::from_secs(1),
            |_| (),
        ),
        Err(SubmitError::ShuttingDown)
    ));

    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_cancelled_before_scheduling_never_runs_its_job() {
    let (runtime, mut events) = Runtime::<()>::new();
    let gate = PublicationGate::default();
    let request = gate.begin(RequestSlot::new(1), &runtime.cancellation_root());
    let started = Arc::new(AtomicUsize::new(0));
    let job_started = Arc::clone(&started);

    request.cancel();
    runtime
        .submit_worker(request, Duration::from_secs(10), move |_| {
            job_started.store(1, Ordering::Release);
        })
        .unwrap();

    let event = events.recv().await.unwrap();
    assert_eq!(event.kind, WorkKind::Worker);
    assert!(matches!(event.result, Err(RuntimeError::Cancelled)));
    runtime.shutdown().await;
    assert_eq!(started.load(Ordering::Acquire), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_cancelled_during_work_publishes_no_value() {
    let (runtime, mut events) = Runtime::new();
    let gate = PublicationGate::default();
    let request = gate.begin(RequestSlot::new(1), &runtime.cancellation_root());

    runtime
        .submit_worker(request.clone(), Duration::from_secs(10), |cancellation| {
            until_cancelled(cancellation);
            7_u32
        })
        .unwrap();
    request.cancel();

    assert!(matches!(
        events.recv().await.unwrap().result,
        Err(RuntimeError::Cancelled)
    ));
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_worker_deadline_reports_timeout_and_waits_for_cleanup() {
    let (runtime, mut events) = Runtime::new();
    let gate = PublicationGate::default();
    let request = gate.begin(RequestSlot::new(1), &runtime.cancellation_root());
    let cleaned = Arc::new(AtomicUsize::new(0));
    let job_cleaned = Arc::clone(&cleaned);

    runtime
        .submit_worker(request, Duration::from_millis(5), move |_| {
            std::thread::sleep(Duration::from_millis(30));
            job_cleaned.store(1, Ordering::Release);
            7_u32
        })
        .unwrap();

    assert!(matches!(
        events.recv().await.unwrap().result,
        Err(RuntimeError::Timeout)
    ));
    runtime.shutdown().await;
    assert_eq!(cleaned.load(Ordering::Acquire), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_owned_work_and_waits_for_cleanup() {
    let (runtime, _events) = Runtime::<()>::new();
    let gate = PublicationGate::default();
    let started = Arc::new(AtomicUsize::new(0));
    let cleaned = Arc::new(AtomicUsize::new(0));
    let job_started = Arc::clone(&started);
    let job_cleaned = Arc::clone(&cleaned);
    // An independent root proves that shutdown cancels the work itself, not
    // only the requests that share the runtime token.
    let independent_root = CancellationToken::new();
    let request = gate.begin(RequestSlot::new(1), &independent_root);

    runtime
        .submit_worker(request, Duration::from_secs(10), move |cancellation| {
            job_started.store(1, Ordering::Release);
            until_cancelled(cancellation);
            job_cleaned.store(1, Ordering::Release);
        })
        .unwrap();
    // Wait until the job holds the worker thread, so the shutdown below finds
    // work that already started.
    while started.load(Ordering::Acquire) == 0 {
        tokio::task::yield_now().await;
    }

    runtime.shutdown().await;
    assert_eq!(cleaned.load(Ordering::Acquire), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gate_rejects_the_result_of_a_superseded_request() {
    let (runtime, mut events) = Runtime::new();
    let gate = PublicationGate::default();
    let root = runtime.cancellation_root();
    let slot = RequestSlot::new(1);
    let stale = gate.begin(slot, &root);

    runtime
        .submit_worker(stale, Duration::from_secs(10), |cancellation| {
            until_cancelled(cancellation);
            7_u32
        })
        .unwrap();
    let newest = gate.begin(slot, &root);

    let event = events.recv().await.unwrap();
    assert!(!gate.accepts(&event.request));
    assert!(gate.accepts(&newest));
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_process_service_captures_bounded_output() {
    let (runtime, mut events) = Runtime::new();
    let gate = PublicationGate::default();
    let request = gate.begin(RequestSlot::new(1), &runtime.cancellation_root());

    runtime
        .submit_process(
            request,
            shell("printf stdout; printf stderr >&2"),
            |output| output,
        )
        .unwrap();

    let event = events.recv().await.unwrap();
    assert_eq!(event.kind, WorkKind::Process);
    let output = event.result.unwrap();
    assert_eq!(output.status_code, Some(0));
    assert_eq!(output.stdout, b"stdout");
    assert_eq!(output.stderr, b"stderr");
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_process_deadline_and_the_shared_output_limit_are_typed() {
    let (runtime, mut events) = Runtime::<()>::new();
    let gate = PublicationGate::default();
    let root = runtime.cancellation_root();

    let mut slow = shell("sleep 10");
    slow.deadline = Duration::from_millis(5);
    runtime
        .submit_process(gate.begin(RequestSlot::new(1), &root), slow, |_| ())
        .unwrap();
    assert!(matches!(
        events.recv().await.unwrap().result,
        Err(RuntimeError::Timeout)
    ));

    // Five bytes on each stream exceed the limit that both streams share.
    let mut noisy = shell("printf 12345; printf 67890 >&2");
    noisy.output_bytes_max = 8;
    runtime
        .submit_process(gate.begin(RequestSlot::new(2), &root), noisy, |_| ())
        .unwrap();
    assert!(matches!(
        events.recv().await.unwrap().result,
        Err(RuntimeError::OutputLimit { limit: 8 })
    ));

    runtime.shutdown().await;
}

/// The job that one masking test runs and then releases.
///
/// The pair reports when the blocking job entered its commit and holds it there
/// until the test releases it, so a cancellation always reaches a running job.
struct CommitSeam {
    entered: std::sync::mpsc::Receiver<()>,
    release: std::sync::mpsc::Sender<()>,
}

/// Creates one paused job and the seam that controls it.
fn paused_job() -> (
    CommitSeam,
    impl FnOnce(CancellationToken) -> u32 + Send + 'static,
) {
    let (entered_sender, entered) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let job = move |_cancellation: CancellationToken| {
        entered_sender
            .send(())
            .expect("the test waits for this report");
        released.recv().expect("the test releases this job");
        COMMITTED_VALUE
    };
    (CommitSeam { entered, release }, job)
}

/// The value that one released job returns.
const COMMITTED_VALUE: u32 = 7;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_committing_job_reports_the_value_that_it_committed() {
    let (runtime, mut events) = Runtime::<u32>::with_limits(RuntimeLimits::new(8, 2, 2).unwrap());
    let gate = PublicationGate::default();
    let request = gate.begin(RequestSlot::new(1), &runtime.cancellation_root());
    let (seam, job) = paused_job();

    runtime
        .submit_committing_worker(request.clone(), Duration::from_secs(30), job)
        .unwrap();
    seam.entered.recv().expect("the job entered its commit");
    // The cancellation reaches a job that already changed durable state, so the
    // caller must still learn its outcome.
    request.cancel();
    seam.release.send(()).unwrap();

    let event = events.recv().await.expect("the request keeps its slot");
    assert_eq!(event.result.unwrap(), COMMITTED_VALUE);
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_optional_job_loses_the_value_that_a_cancellation_displaced() {
    let (runtime, mut events) = Runtime::<u32>::with_limits(RuntimeLimits::new(8, 2, 2).unwrap());
    let gate = PublicationGate::default();
    let request = gate.begin(RequestSlot::new(1), &runtime.cancellation_root());
    let (seam, job) = paused_job();

    runtime
        .submit_worker(request.clone(), Duration::from_secs(30), job)
        .unwrap();
    seam.entered.recv().expect("the job entered its work");
    request.cancel();
    seam.release.send(()).unwrap();

    let event = events.recv().await.expect("the request keeps its slot");
    assert!(matches!(event.result, Err(RuntimeError::Cancelled)));
    assert_eq!(event.kind, WorkKind::Worker);
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_started_shutdown_keeps_the_tracked_tasks_of_its_drain() {
    let (runtime, mut events) = Runtime::<u32>::with_limits(RuntimeLimits::new(8, 2, 2).unwrap());
    let gate = PublicationGate::default();
    let request = gate.begin(RequestSlot::new(1), &runtime.cancellation_root());
    let (seam, job) = paused_job();

    runtime
        .submit_committing_worker(request, Duration::from_secs(30), job)
        .unwrap();
    seam.entered.recv().expect("the job entered its commit");
    let drain = runtime.begin_shutdown();

    // The deadline expires while the job still holds its commit, and the drain
    // keeps that job for the caller that returns to the wait.
    assert!(
        tokio::time::timeout(Duration::ZERO, drain.wait())
            .await
            .is_err()
    );
    assert!(!drain.is_empty(), "the drain still owns the paused job");
    seam.release.send(()).unwrap();
    drain.wait().await;

    let event = events.recv().await.expect("the request keeps its slot");
    assert_eq!(event.result.unwrap(), COMMITTED_VALUE);
}
