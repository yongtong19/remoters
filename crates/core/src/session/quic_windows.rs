use std::{
    ffi::c_void,
    future::Future,
    io,
    net::UdpSocket,
    pin::Pin,
    ptr::{null, null_mut},
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
    time::Instant,
};

use quinn::{AsyncTimer, AsyncUdpSocket, Runtime, TokioRuntime};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::Threading::{
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CloseThreadpoolWait, CreateThreadpoolWait,
        CreateWaitableTimerExW, PTP_CALLBACK_INSTANCE, PTP_WAIT, SYNCHRONIZATION_SYNCHRONIZE,
        SetThreadpoolWait, SetWaitableTimer, TIMER_MODIFY_STATE, WaitForThreadpoolWaitCallbacks,
    },
};

/// Quinn's send pacing needs a finer wakeup than the default Windows sleep.
#[derive(Debug)]
pub(super) struct WindowsRuntime;

impl Runtime for WindowsRuntime {
    fn new_timer(&self, deadline: Instant) -> Pin<Box<dyn AsyncTimer>> {
        match HighResolutionTimer::new(deadline) {
            Some(timer) => Box::pin(timer),
            None => TokioRuntime.new_timer(deadline),
        }
    }

    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        TokioRuntime.spawn(future);
    }

    fn wrap_udp_socket(&self, socket: UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        TokioRuntime.wrap_udp_socket(socket)
    }
}

#[derive(Debug, Default)]
struct Signal {
    fired: bool,
    waker: Option<Waker>,
}

#[derive(Debug)]
struct Wait {
    timer: HANDLE,
    registration: PTP_WAIT,
    signal: Box<Mutex<Signal>>,
}

// The timer handle and registration can be used across threads. The Rust
// owner serializes all Win32 calls. The callback only accesses the mutex.
unsafe impl Send for Wait {}

impl Wait {
    fn new() -> Option<Self> {
        let timer = unsafe {
            CreateWaitableTimerExW(
                null(),
                null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE,
            )
        };
        if timer.is_null() {
            return None;
        }
        let mut signal = Box::<Mutex<Signal>>::default();
        let registration = unsafe {
            CreateThreadpoolWait(
                Some(fired),
                std::ptr::from_mut(signal.as_mut()).cast(),
                null(),
            )
        };
        if registration == 0 {
            unsafe { CloseHandle(timer) };
            return None;
        }
        Some(Self {
            timer,
            registration,
            signal,
        })
    }

    fn arm(&self, deadline: Instant) -> bool {
        let nanos = deadline
            .saturating_duration_since(Instant::now())
            .as_nanos();
        let ticks = nanos.div_ceil(100).clamp(1, i64::MAX as u128) as i64;
        unsafe {
            SetThreadpoolWait(self.registration, null_mut(), null());
            // A negative due time is relative and measured in 100 ns units.
            if SetWaitableTimer(self.timer, &-ticks, 0, None, null(), 0) == 0 {
                return false;
            }
            SetThreadpoolWait(self.registration, self.timer, null());
        }
        true
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        unsafe {
            SetThreadpoolWait(self.registration, null_mut(), null());
            WaitForThreadpoolWaitCallbacks(self.registration, 1);
            CloseThreadpoolWait(self.registration);
            CloseHandle(self.timer);
        }
    }
}

unsafe extern "system" fn fired(
    _: PTP_CALLBACK_INSTANCE,
    context: *mut c_void,
    _: PTP_WAIT,
    _: u32,
) {
    // The owner waits for callbacks before releasing this allocation.
    let signal = unsafe { &*context.cast::<Mutex<Signal>>() };
    let waker = {
        let mut signal = signal.lock().unwrap();
        signal.fired = true;
        signal.waker.take()
    };
    if let Some(waker) = waker {
        waker.wake();
    }
}

#[derive(Debug)]
enum TimerBackend {
    Native { wait: Wait, armed: bool },
    Tokio(Pin<Box<dyn AsyncTimer>>),
}

#[derive(Debug)]
struct HighResolutionTimer {
    deadline: Instant,
    backend: TimerBackend,
}

impl HighResolutionTimer {
    fn new(deadline: Instant) -> Option<Self> {
        Some(Self {
            deadline,
            backend: TimerBackend::Native {
                wait: Wait::new()?,
                armed: false,
            },
        })
    }
}

impl AsyncTimer for HighResolutionTimer {
    fn reset(mut self: Pin<&mut Self>, deadline: Instant) {
        self.deadline = deadline;
        match &mut self.backend {
            TimerBackend::Native { armed, .. } => *armed = false,
            TimerBackend::Tokio(timer) => timer.as_mut().reset(deadline),
        }
    }

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let deadline = self.deadline;
        if deadline <= Instant::now() {
            return Poll::Ready(());
        }
        if let TimerBackend::Native { wait, armed } = &mut self.backend {
            {
                let mut signal = wait.signal.lock().unwrap();
                if !signal
                    .waker
                    .as_ref()
                    .is_some_and(|old| old.will_wake(cx.waker()))
                {
                    signal.waker = Some(cx.waker().clone());
                }
                if *armed && !signal.fired {
                    return Poll::Pending;
                }
                signal.fired = false;
            }
            if wait.arm(deadline) {
                *armed = true;
                return Poll::Pending;
            }
            self.backend = TimerBackend::Tokio(TokioRuntime.new_timer(deadline));
        }
        let TimerBackend::Tokio(timer) = &mut self.backend else {
            unreachable!()
        };
        timer.as_mut().poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::poll_fn, time::Duration};

    #[tokio::test]
    async fn timer_wakes_after_being_moved_to_an_earlier_deadline() {
        let now = Instant::now();
        let Some(timer) = HighResolutionTimer::new(now + Duration::from_secs(5)) else {
            return;
        };
        let mut timer = Box::pin(timer);
        poll_fn(|cx| {
            assert!(timer.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        for delay in [Duration::from_millis(5), Duration::from_millis(3)] {
            let deadline = Instant::now() + delay;
            timer.as_mut().reset(deadline);
            tokio::time::timeout(
                Duration::from_secs(1),
                poll_fn(|cx| timer.as_mut().poll(cx)),
            )
            .await
            .expect("The Windows timer did not wake the connection task.");
            assert!(Instant::now() >= deadline);
        }
    }
}
