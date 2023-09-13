use crate::shared::{initializible::Initializible, priority::Priority};
use crate::async_stream::stream::AsyncStream;
use async_std::sync::Mutex;
use async_std::task::JoinHandle;
use num_cpus;
use std::{future::Future, sync::Arc};
use threadpool::ThreadPool;

type Lock = Arc<Mutex<Vec<(Priority, JoinHandle<()>)>>>;

pub struct RuntimeEngine<ItemType> {
    pub(crate) iter: Lock,
    pub(crate) engine: ThreadPool,
    pub stream: AsyncStream<ItemType>,
}

impl<ItemType> Initializible for RuntimeEngine<ItemType> {
    fn init() -> Self {
        let thread_count = num_cpus::get();
        let engine = threadpool::Builder::new()
            .num_threads(thread_count)
            .thread_name("RuntimeEngine".to_owned())
            .build();
        Self {
            engine,
            iter: Arc::new(Mutex::new(vec![])),
            stream: AsyncStream::new(),
        }
    }
}

impl<ItemType> RuntimeEngine<ItemType> {
    pub fn cancel(&mut self) {
        let lock = self.iter.clone();
        let stream = self.stream.clone();
        let task = async move {
            let mut iter = lock.lock().await;
            while let Some((_, handle)) = iter.pop() {
                _ = handle.cancel().await;
            }
        };
        self.engine.execute(|| {
            async_std::task::block_on(task);
        });
        stream.cancel_tasks();
        self.poll();
    }
}

impl<ItemType: Send + 'static> RuntimeEngine<ItemType> {
    pub fn write_task<F>(&mut self, priority: Priority, task: F)
    where
        F: Future<Output = ItemType> + Send + 'static,
    {
        let mut stream = self.stream.clone();
        let task = async_std::task::spawn(async move {
            stream.increment().await;
            stream.insert_item(task.await).await;
            stream.decrement_task_count().await;
        });
        let lock = self.iter.clone();
        self.engine.execute(move || {
            async_std::task::block_on(async move {
                let mut iter = lock.lock().await;
                iter.push((priority, task));
            });
        });
    }
}

impl<ItemType: Send + 'static> RuntimeEngine<ItemType> {
    pub async fn wait_for_all_tasks(&mut self) {
        let lock = self.iter.clone();
        self.poll();
        let stream = self.stream.clone();
        let task_count = self.stream.clone().task_count();
        let engine = self.engine.clone();
        if stream.is_empty().await || task_count == 0 {
            return;
        }
        let mut iter = lock.lock().await;
        iter.sort_by(|lhs, rhs| lhs.0.cmp(&rhs.0));
        while let Some((_, handle)) = iter.pop() {
            engine.execute(|| {
                async_std::task::block_on(async move {
                    handle.await;
                });
            });
        }
        self.poll();
    }

    pub(crate) fn wait_for(&self, count: usize) {
        let lock = self.iter.clone();
        self.poll();
        let stream = self.stream.clone();
        let task_count = self.stream.clone().task_count();
        let engine = self.engine.clone();
        _ = std::thread::spawn(move || {
            async_std::task::block_on(async move {
                if stream.is_empty().await || task_count == 0 {
                    return;
                }
                let mut iter = lock.lock().await;
                if count < task_count {
                    return;
                }
                if count > iter.len() {
                    return;
                }
                let mut count = count;
                iter.sort_by(|lhs, rhs| lhs.0.cmp(&rhs.0));
                while count != 0 {
                    if let Some((_, handle)) = iter.pop() {
                        engine.execute(|| {
                            async_std::task::block_on(async move {
                                handle.await;
                            });
                        });
                    }
                    count -= 1;
                }
            });
        }).join();
        self.poll();
    }
}

impl<ItemType> RuntimeEngine<ItemType> {
    fn poll(&self) {
        self.engine.join();
    }
}

impl<ItemType> Drop for RuntimeEngine<ItemType> {
    fn drop(&mut self) {
        self.poll();
    }
}

impl<ItemType> Clone for RuntimeEngine<ItemType> {
    fn clone(&self) -> Self {
        Self {
            iter: self.iter.clone(),
            engine: threadpool::Builder::new()
                .num_threads(num_cpus::get())
                .thread_name("RuntimeEngine".to_owned())
                .build(),
            stream: self.stream.clone(),
        }
    }
}
