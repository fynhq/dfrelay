use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A future adapter that catches panics raised while polling the inner future.
///
/// `tokio::spawn` captures a panic into the returned `JoinHandle`; when that
/// handle is dropped the panic is silently discarded. For daemon background
/// tasks that only report completion through an event channel, a panic would
/// leave their in-flight flags set forever. Wrapping the future lets the caller
/// observe the panic and emit a failure event instead.
pub struct CatchUnwind<F: Future> {
    inner: Pin<Box<F>>,
}

impl<F: Future> CatchUnwind<F> {
    pub fn new(inner: F) -> Self {
        Self {
            inner: Box::pin(inner),
        }
    }
}

impl<F: Future> Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn Any + Send>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().get_mut();
        match std::panic::catch_unwind(AssertUnwindSafe(|| this.inner.as_mut().poll(cx))) {
            Ok(poll) => poll.map(Ok),
            Err(payload) => Poll::Ready(Err(payload)),
        }
    }
}

/// Best-effort human-readable text for a panic payload.
pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_panic_in_async_block() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let result = runtime.block_on(async {
            CatchUnwind::new(async {
                panic!("boom");
            })
            .await
        });
        let payload = result.expect_err("panic should be caught");
        assert_eq!(panic_message(payload.as_ref()), "boom");
    }

    #[test]
    fn passes_through_normal_completion() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let result = runtime.block_on(CatchUnwind::new(async { 7u8 }));
        assert_eq!(result.ok(), Some(7));
    }
}
