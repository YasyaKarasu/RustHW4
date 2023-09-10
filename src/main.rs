use std::{
    cell::RefCell,
    future::Future,
    sync::{Arc, Condvar, Mutex},
    task::{Context, Poll, RawWaker, RawWakerVTable, Wake, Waker},
    collections::VecDeque,
};
use futures::future::BoxFuture;
use scoped_tls::scoped_thread_local;

fn dummy_waker() -> Waker {
    static DATA: () = ();
    unsafe { Waker::from_raw(RawWaker::new(&DATA, &VTABLE)) }
}

const VTABLE: RawWakerVTable = RawWakerVTable::new(vtable_clone, vtable_wake, vtable_wake_by_ref, vtable_drop);

unsafe fn vtable_clone(_p: *const()) -> RawWaker {
    eprintln!("vtable_clone");
    RawWaker::new(_p, &VTABLE)
}

unsafe fn vtable_wake(_p: *const()) {
    eprintln!("vtable_wake");
}

unsafe fn vtable_wake_by_ref(_p: *const()) {
    eprintln!("vtable_wake_by_ref");
}

unsafe fn vtable_drop(_p: *const()) {
    eprintln!("vtable_drop");
}

struct Signal {
    state: Mutex<State>,
    cond: Condvar,
}

enum State {
    Empty,
    Waiting,
    Notified,
}

impl Signal {
    fn wait(&self) {
        let mut state = self.state.lock().unwrap();
        match *state {
            State::Notified => {
                *state = State::Empty;
                return;
            }
            State::Waiting => {
                panic!("Cannot wait twice");
            }
            State::Empty => {
                *state = State::Waiting;
                while let State::Waiting = *state {
                    state = self.cond.wait(state).unwrap();
                }
            }
        }
    }

    fn notify(&self) {
        let mut state = self.state.lock().unwrap();
        match *state {
            State::Empty => {
                *state = State::Notified;
            }
            State::Waiting => {
                *state = State::Empty;
                self.cond.notify_one();
            }
            State::Notified => {
                panic!("Cannot notify twice");
            }
        }
    }
}

impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.notify();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notify();
    }
}

struct Task {
    future: RefCell<BoxFuture<'static, ()>>,
    signal: Arc<Signal>,
}
unsafe impl Send for Task {}
unsafe impl Sync for Task {}

scoped_thread_local!(static SIGNAL: Arc<Signal>);
scoped_thread_local!(static RUNNABLE: Mutex<VecDeque<Arc<Task>>>);

impl Wake for Task {
    fn wake(self: Arc<Self>) {
        RUNNABLE.with(|runnable| {
            runnable.lock().unwrap().push_back(self.clone());
        });
        SIGNAL.with(|signal| {
            signal.notify();
        });
    }

    fn wake_by_ref(self: &Arc<Self>) {
        RUNNABLE.with(|runnable| {
            runnable.lock().unwrap().push_back(self.clone());
        });
        SIGNAL.with(|signal| {
            signal.notify();
        });
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut fut = std::pin::pin!(future);
    let signal = Arc::new(Signal {
        state: Mutex::new(State::Empty),
        cond: Condvar::new(),
    });
    let waker = Waker::from(signal.clone());
    let mut cx = Context::from_waker(&waker);
    
    let runnable = Mutex::new(VecDeque::with_capacity(1024));
    SIGNAL.set(&signal, || {
        RUNNABLE.set(&runnable, || {
            loop {
                if let Poll::Ready(val) = fut.as_mut().poll(&mut cx) {
                    return val;
                }
                while let Some(task) = runnable.lock().unwrap().pop_front() {
                    let waker = Waker::from(task.clone());
                    let mut cx = Context::from_waker(&waker);
                    let _ = task.future.borrow_mut().as_mut().poll(&mut cx);
                }
                signal.wait();
            }
        })
    })
}

fn my_spawn<F: Future<Output = ()> + 'static + Send>(future: F) {
    let task = Arc::new(Task {
        future: RefCell::new(Box::pin(future)),
        signal: Arc::new(Signal {
            state: Mutex::new(State::Empty),
            cond: Condvar::new(),
        }),
    });
    let waker = Waker::from(task.clone());
    let mut cx = Context::from_waker(&waker);
    if let Poll::Ready(_)= task.future.borrow_mut().as_mut().poll(&mut cx) {
        return;
    }
    RUNNABLE.with(|runnable| {
        runnable.lock().unwrap().push_back(task);
    });
}

async fn demo() {
    let (tx, rx) = async_channel::bounded(1);
    my_spawn(demo2(tx));
    println!("Hello from demo");
    let _ = rx.recv().await;
}

async fn demo2(tx: async_channel::Sender<()>) {
    println!("Hello from demo2");
    let _ = tx.send(()).await;
}

fn main() {
    block_on(demo());
}