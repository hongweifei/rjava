//! Bridge a Java event/listener callback to a Rust [`Future`] — std-only:
//! no executor dependency, no `unsafe`.
//!
//! [`crate::rx::from_callback()`] turns a Java event source into an
//! awaitable Rust future: you build a listener as an
//! [`interface::proxy`](crate::interface::proxy) handler, register it with a
//! Java host (e.g. an `addListener(...)` call), and the returned future
//! resolves when the handler fires. This is the mirror of
//! [`crate::future::java_future()`] for **callback**-style (push) events,
//! where Java delivers the value on its own thread instead of Rust polling a
//! `CompletableFuture`.
//!
//! # Usage
//!
//! ```no_run
//! use std::sync::Arc;
//! use rjava::interface::{self, Call};
//! use rjava::prelude::*;
//! use rjava::rx;
//!
//! # async fn example(java: Java) -> JavaResult<()> {
//! // Java side (a fixture — the interface is ordinary Java):
//! //   public interface Listener { void onEvent(String value); }
//! //   public class Host { Host(Listener l); void fire(String v); }
//! // 1) build the listener: the closure converts each call into the
//! //    future's value and sends it; `interface::null()` is the void return
//! let (listener, event) = rx::from_callback::<String, _>(&["Listener"], |tx| {
//!     Arc::new(move |env, call: Call| {
//!         if call.name == "onEvent" {
//!             let value = String::from_java(env, call.args.into_iter().next().expect("onEvent takes 1 arg"))?;
//!             let _ = tx.send(Ok(value));
//!         }
//!         Ok(interface::null())
//!     })
//! })?;
//!
//! // 2) register the listener with the Java host
//! let host: JObject = java.new_object("Host", (&listener,))?;
//! host.call_void("fire", ("hello from Java",))?;
//!
//! // 3) await the first event
//! let value: String = event.await?;
//! # assert_eq!(value, "hello from Java");
//! # Ok(())
//! # }
//! ```
//!
//! # Semantics
//!
//! * **One-shot.** The future completes with the **first** value the handler
//!   sends; later sends are dropped (the channel closes when the worker
//!   thread exits after delivering the first value). This makes
//!   `from_callback` a natural fit for one-shot listeners (a completion
//!   callback, a `Consumer` handed to a single-use API); for multi-event
//!   streams, create one bridge per event you await, or drive a channel
//!   yourself inside the handler.
//! * **Lazy start.** Nothing is spawned until the first poll; constructing
//!   the bridge is cheap and the proxy is created eagerly (so the
//!   registration call can fail fast). The worker is driven entirely by the
//!   executor: the first poll spawns one detached std thread that blocks on
//!   the channel and wakes the future's waker when a value (or the channel
//!   closing) arrives.
//! * **Send.** The returned future is `Send` (its state is a `Mutex`; the
//!   receiver is moved onto the worker thread), so it can be awaited from any
//!   executor thread.
//! * **Fused.** Once `Ready`, the future stays completed; polling it again
//!   after completion is a contract violation and panics.
//! * **Cancellation.** Dropping the future does **not** unregister the
//!   listener: if the worker thread was already spawned it keeps waiting on
//!   the channel (a future that is dropped *before* its first poll spawns
//!   nothing — the receiver is dropped and the handler's later `send` calls
//!   fail with [`SendError`](std::sync::mpsc::SendError), which a handler can
//!   observe). Cancel from the Java side if needed (remove the listener, or
//!   make the host stop firing).
//! * **Timeouts.** Not provided here — race the Java side like
//!   [`crate::future`] recommends (e.g. have the host fire an error event,
//!   or `select!` over a timer of your own), since this module is
//!   executor-agnostic.
//! * **Channel closed without a value.** The handler (and therefore the
//!   sender) is released when the proxy becomes unreachable (Java garbage
//!   collection) or when the bridge was never registered with a Java host.
//!   A poll that observes the closed channel — i.e. the worker thread's
//!   `recv()` returned `Err` — resolves the future with a
//!   [`JavaError::InvalidArgument`] naming the condition. Note the worker
//!   thread **ends** when the channel closes, so a dropped-but-polled future
//!   with a never-firing listener resolves this way, while a dropped-and-
//!   never-polled future (no thread) leaves the send errors to the handler.
//!
//! # Feature requirement
//!
//! `rx` builds the listener with [`interface::proxy`](crate::interface::proxy),
//! so the module exists only with the `interface` feature (default off),
//! like [`crate::interface`](mod@crate::interface) itself. The `F` closure
//! receives the
//! [`std::sync::mpsc::Sender`] and returns the
//! [`Arc<interface::Handler>`](crate::interface::Handler); `from_callback`
//! itself needs no [`Java`](crate::Java) facade — the proxy is created via
//! the thread-local attachment machinery, and the worker thread never
//! touches the JVM (it only blocks on the channel), so there is nothing for
//! a facade to do.

use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use crate::convert::FromJava;
use crate::error::{JavaError, JavaResult};
use crate::handles::JObject;
use crate::interface::{self, Handler};

/// The shared state of a [`from_callback()`] bridge, guarded by a `Mutex`.
///
/// The worker thread and the polling side communicate exclusively through
/// this state, mirroring [`crate::future`]'s bridge: the poller spawns the
/// worker out of [`State::Unstarted`] and observes the outcome in
/// [`State::Done`]; the worker blocks on the channel, stores the outcome,
/// and wakes the last registered waker. Both transitions happen under the
/// same lock, so a poll that observes [`State::Waiting`] can never miss the
/// completion.
enum State<T> {
    /// Not yet polled: the channel receiver to block on.
    Unstarted {
        /// The receiving end of the channel the handler sends through.
        rx: Receiver<JavaResult<T>>,
    },
    /// The worker is running (or about to be spawned); `waker` is the latest
    /// waker registered by a poll.
    Waiting {
        /// The waker to wake when the outcome is ready.
        waker: Option<Waker>,
    },
    /// The outcome is ready. `None` means it has already been returned by a
    /// poll (the future is then spent — polling again is a bug).
    Done(Option<JavaResult<T>>),
}

/// The future returned by [`from_callback()`]. Private: the public
/// constructor is the function, and the concrete type is exposed as
/// `impl Future`.
struct CallbackFuture<T> {
    /// Shared state; cloned into the worker thread at spawn.
    state: Arc<Mutex<State<T>>>,
}

/// Bridge a Java event/listener callback to a Rust future.
///
/// The first poll spawns one detached std thread that blocks on a
/// `std::sync::mpsc` channel; the thread wakes the future when the handler
/// fires (or the channel closes). See the [module docs](crate::rx) for the
/// full semantics (one-shot, lazy start, cancellation, timeouts).
///
/// # Parameters
///
/// * `ifaces` — the binary names of the Java interfaces the listener
///   implements (passed to [`interface::proxy`]); at least one is required.
/// * `f` — called **once**, synchronously, with the
///   [`Sender`] the handler sends through. It must return the
///   [`Arc<Handler>`](crate::interface::Handler) closure implementing the
///   listener: on the event you await, convert the
///   [`Call`](crate::interface::Call) arguments to `T` with
///   [`FromJava`] and `tx.send(Ok(value))` (ignore the
///   `SendError` — it merely means the future was already completed or
///   dropped); return [`interface::null()`] for `void` methods.
///
/// # Return value
///
/// `(proxy, future)` — the [`JObject`] proxy to register with the Java host
/// (an `addListener(...)` argument, a `thenAccept(...)` argument, …), and
/// the future that resolves with the first value the handler sends.
///
/// # Errors
///
/// Everything [`interface::proxy`] can fail with (the proxy is created
/// eagerly, before the future is returned): a missing JVM, a bootstrap
/// failure, an unresolvable interface name, an empty `ifaces` list, …
/// Errors the handler itself produces are thrown into the *calling* Java
/// code at event time, like any other handler error.
///
/// ```no_run
/// # use std::sync::Arc;
/// # use rjava::interface::{self, Call};
/// # use rjava::prelude::*;
/// # use rjava::rx;
/// # async fn example() -> JavaResult<()> {
/// let (listener, event) = rx::from_callback::<String, _>(&["Listener"], |tx| {
///     Arc::new(move |env, call: Call| {
///         if call.name == "onEvent" {
///             let value = String::from_java(env, call.args.into_iter().next().expect("onEvent takes 1 arg"))?;
///             let _ = tx.send(Ok(value));
///         }
///         Ok(interface::null())
///     })
/// })?;
/// # let _ = (listener, event);
/// # Ok(())
/// # }
/// ```
pub fn from_callback<T, F>(
    ifaces: &[impl AsRef<str>],
    f: F,
) -> JavaResult<(JObject, impl Future<Output = JavaResult<T>> + Send)>
where
    T: FromJava + Send + 'static,
    F: FnOnce(Sender<JavaResult<T>>) -> Arc<Handler>,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let handler = f(tx);
    let proxy = interface::proxy(handler, ifaces)?;
    let future = CallbackFuture {
        state: Arc::new(Mutex::new(State::Unstarted { rx })),
    };
    Ok((proxy, future))
}

impl<T: Send + 'static> Future for CallbackFuture<T> {
    type Output = JavaResult<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Clone the Arc first so the spawn closure can own a handle to the
        // same state even while the guard below borrows it.
        let state_arc = Arc::clone(&self.state);
        let mut state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            // First poll: take the receiver out, spawn the worker, and
            // re-enter the loop to register the waker (now `Waiting`).
            if matches!(*state, State::Unstarted { .. }) {
                let State::Unstarted { rx } =
                    std::mem::replace(&mut *state, State::Waiting { waker: None })
                else {
                    unreachable!("guarded by the matches! above")
                };
                let worker_state = Arc::clone(&state_arc);
                let spawned = std::thread::Builder::new()
                    .name("rjava-rx-callback".into())
                    .spawn(move || worker_thread(rx, worker_state));
                if spawned.is_err() {
                    // The worker can never run; the future can never complete
                    // normally, so surface the failure as the outcome.
                    let err = JavaError::InvalidArgument(
                        "failed to spawn the callback bridge thread",
                    );
                    *state = State::Done(Some(Err(JavaError::InvalidArgument(
                        "failed to spawn the callback bridge thread",
                    ))));
                    return Poll::Ready(Err(err));
                }
                continue;
            }
            match &mut *state {
                State::Waiting { waker } => {
                    let should_update = match waker {
                        Some(current) => !current.will_wake(cx.waker()),
                        None => true,
                    };
                    if should_update {
                        *waker = Some(cx.waker().clone());
                    }
                    return Poll::Pending;
                }
                State::Done(outcome) => match outcome.take() {
                    Some(outcome) => return Poll::Ready(outcome),
                    None => panic!("CallbackFuture polled after completion"),
                },
                State::Unstarted { .. } => unreachable!("handled by the if-let above"),
            }
        }
    }
}

/// The worker thread body: block on the channel, store the outcome, and
/// wake the registered waker (outside the lock — a waker may re-enter poll
/// synchronously).
fn worker_thread<T: Send + 'static>(rx: Receiver<JavaResult<T>>, state: Arc<Mutex<State<T>>>) {
    let outcome = match rx.recv() {
        Ok(outcome) => outcome,
        Err(_) => Err(JavaError::InvalidArgument(
            "the callback's handler was dropped without ever firing (the \
             listener was never registered in Java, or it was \
             garbage-collected before the event)",
        )),
    };
    let waker = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        let waker = match &mut *state {
            State::Waiting { waker } => waker.take(),
            _ => None,
        };
        *state = State::Done(Some(outcome));
        waker
    };
    if let Some(waker) = waker {
        waker.wake();
    }
}
