use tokio::task::JoinHandle;
use crate::log::log_message;

/// Hard cap on concurrent background tasks.
const MAX_SPAWNED_TASKS: usize = 10;

/// Clean up finished tasks from a vector of JoinHandles.
pub fn cleanup_tasks(tasks: &mut Vec<JoinHandle<()>>) {
    tasks.retain(|h| !h.is_finished());
}

/// Spawn a task while respecting the MAX_SPAWNED_TASKS limit.
/// Automatically cleans up completed tasks before spawning.
pub fn spawn_task_limited<F>(tasks: &mut Vec<JoinHandle<()>>, fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    cleanup_tasks(tasks);

    if tasks.len() < MAX_SPAWNED_TASKS {
        tasks.push(tokio::spawn(fut));
    } else {
        log_message("Max spawned tasks reached, skipping task spawn");
    }
}
