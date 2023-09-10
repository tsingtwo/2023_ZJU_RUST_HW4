use std::{thread:: JoinHandle, cell::RefCell, sync::{Arc, Mutex, Condvar, mpsc}, future::Future,  collections::VecDeque, task::{Waker, Context, Poll, Wake, RawWaker, RawWakerVTable}};
use futures::future::BoxFuture;
use scoped_tls::scoped_thread_local;

fn main(){
    // m_block_on(demo());
    let exe = Executor::new();
    exe.block_on(demo());
}

async fn demo() {
    let (tx, rx) = async_channel::bounded::<()>(2);
    Executor::spawn(demo2(tx));
    println!("Hello World");
    let _ = rx.recv().await;
}
async fn demo2(tx: async_channel::Sender<()>){
    println!("hello world2!");
    let _ = tx.send(()).await;
}
scoped_tls::scoped_thread_local!(static SIGNAL: Arc<Signal>);
scoped_tls::scoped_thread_local!(static RUNNABLE: Mutex<VecDeque<Arc<Task>>>);

struct Demo;
// #[allow(unused_variables)]
impl Future for Demo {
    type Output = ();
    fn poll(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        println!("Hello World!");
        std::task::Poll::Ready(())
    }
}

struct  Task {
    future: RefCell<BoxFuture<'static, ()>>,
    signal: Arc<Signal>,
}
unsafe impl  Send for Task {
    
}
unsafe impl Sync for Task{

}
impl Wake for Task{
    fn wake(self: Arc<Self>){
        RUNNABLE.with(|runnable| runnable.lock().unwrap().push_back(self.clone()));
        self.signal.notify();
    }
}

scoped_thread_local!(pub(crate) static EX: Executor);
struct  Executor{
    local_queue: TaskQueue,
    thread_pool: ThreadPool,
}
#[allow(unused)]
impl Executor {
    fn new() -> Executor {
        Executor {
            local_queue: TaskQueue::new(),
            thread_pool: ThreadPool::new(2),
        }
    }
    
    fn spawn(fut: impl Future<Output = ()> + 'static + std::marker::Send) {
        let t = Arc::new(Task {
            future: RefCell::new(Box::pin(fut)),
            signal: Arc::new(Signal::new()),
        });
        EX.with(|ex| ex.local_queue.push(t.clone()));
    }

    fn block_on<F:Future>(&self, future: F) -> F::Output {
        let mut main_fut = std::pin::pin!(future);
        let signal: Arc<Signal> = Arc::new(Signal::new());
        let waker = Waker::from(signal.clone());

        let mut cx = Context::from_waker(&waker);

        EX.set(self, || {
            loop {
                if let std::task::Poll::Ready(t) = main_fut.as_mut().poll(&mut cx) {
                    break t;
                }
                while let Some(t) = self.local_queue.pop() {
                    let _ = self.thread_pool.execute(t);
                }

                // no task to execute now, it may ready
                if let std::task::Poll::Ready(t) = main_fut.as_mut().poll(&mut cx) {
                    break t;
                }
                signal.wait();
            }
        })
    }
}


enum State{
    Empty,
    Waiting,
    Notified,
}

impl Wake for Signal {
    fn wake(self:Arc<Self>){
        self.notify();
    }
}
struct Signal{
    state: Mutex<State>,
    cond: Condvar,
}
impl Signal{
    fn new()-> Self{
        Self{
            state:Mutex::new(State::Empty),
            cond: Condvar::new(),
        }
    }
    fn wait(&self){
        let mut state = self.state.lock().unwrap();
        match *state {
            State::Notified => *state = State::Empty,
            State::Waiting =>{
                panic!("multiple wait");
            }
            State::Empty =>{
                *state =  State::Waiting;
                while let State::Waiting = *state{
                    state= self.cond.wait(state).unwrap();
                }
            }
        }
    }
    fn notify(&self){
        let mut state = self.state.lock().unwrap();
        match *state {
            State::Notified => {}
            State::Empty => *state = State::Notified,
            State::Waiting =>{
                *state = State::Empty;
                self.cond.notify_one();
            }
        }
    }
}

#[allow(unused)]
fn dummy_waker()->Waker{
    static DATA:() = ();
    unsafe { Waker::from_raw(RawWaker::new(&DATA, &VTABLE))}
}
#[allow(unused)]
const VTABLE: RawWakerVTable = RawWakerVTable::new(vtable_clone, vtable_wake, vtable_wake_by_ref, vtable_drop);
#[allow(unused)]
unsafe fn vtable_clone(_p: *const()) -> RawWaker{
    RawWaker::new(_p, &VTABLE)
}
#[allow(unused)]
unsafe fn vtable_wake(_p: *const()) {
    
}
#[allow(unused)]
unsafe fn vtable_wake_by_ref(_p: *const()) {
    
}
#[allow(unused)]
unsafe fn vtable_drop(_p: *const()) {
    
}


struct TaskQueue {
    queue: RefCell<VecDeque<Arc<Task>>>,
}
impl TaskQueue {
    pub fn new() -> Self {
        const DEFAULT_TASK_QUEUE_SIZE: usize = 4096;
        Self::new_with_capacity(DEFAULT_TASK_QUEUE_SIZE)
    }
    pub fn new_with_capacity(capacity: usize) -> Self {
        Self {
            queue: RefCell::new(VecDeque::with_capacity(capacity)),
        }
    }

    pub(crate) fn push(&self, runnable: Arc<Task>) {
        // println!("add task");
        self.queue.borrow_mut().push_back(runnable);
    }

    pub(crate) fn pop(&self) -> Option<Arc<Task>> {
        // println!("remove task");
        self.queue.borrow_mut().pop_front()
    }
}
#[allow(dead_code)]
struct Worker {
    wid: usize,
    wthread: Option<JoinHandle<()>>,
}

impl Worker {
    fn new(id: usize, receiver: Arc::<Mutex<mpsc::Receiver<Option<Arc<Task>>>>>) -> Self {
        // spawn a thread to execute Task
        let thread = std::thread::spawn(move || {
            loop {
                let task = receiver.lock().unwrap().recv().unwrap();
                // println!("worker {} got a task {:?}", id, task.is_some());
                match task {
                    Some(task) => {
                        // println!("worker {} got a task", id);
                        let waker = Waker::from(task.clone());
                        let mut cx = Context::from_waker(&waker);
                        let _ = task.future.borrow_mut().as_mut().poll(&mut cx);
                    },
                    None => {
                        break;
                    },
                }
            }
        });

        Worker { wid: id, wthread: Some(thread) }
    }
}

struct ThreadPool {
    workers: Vec<Worker>,
    max_worker: usize,
    sender: mpsc::Sender<Option<Arc<Task>>>
}

impl ThreadPool {
    fn new(max_worker: usize) -> Self {
        if max_worker == 0 {
            panic!("max_worker must be greater than 0.")
        }
        let (sender,receiver) = mpsc::channel::<Option<Arc<Task>>>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(max_worker);
        for i in 0..max_worker {
            workers.push(Worker::new(i, receiver.clone()));
        }
        ThreadPool { workers, max_worker, sender }
    }

    fn execute(&self, task: Arc<Task>) -> Poll<()> {
        self.sender.send(Some(task)).unwrap();
        Poll::Pending
    }

}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        for _i in 0..self.max_worker {
            let _ = self.sender.send(None);
        }

        for worker in &mut self.workers {
            if let Some(thread) = worker.wthread.take() {
                let _ = thread.join();
            }
        }
    }
}