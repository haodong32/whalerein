//! Test-only driver for concurrent waits in one Core and one authenticated channel.
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use whalerein_core::{Core, CoreOptions, FileSecretStore};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 6 {
        std::process::exit(2);
    }
    let core = Core::new(CoreOptions {
        data_dir: PathBuf::from(&args[1]),
        secrets: Arc::new(FileSecretStore::new(PathBuf::from(&args[2]))),
    })
    .expect("fixture Core");
    let watch_core = core.clone();
    let watch_key = core.session_key(&args[3]).await.expect("fixture session");
    let first_key = watch_key.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let resets = Arc::new(AtomicUsize::new(0));
    let watch_resets = resets.clone();
    let mut watcher = tokio::spawn(async move {
        let mut ready_tx = Some(ready_tx);
        watch_core
            .watch_session(&watch_key, Some(Duration::from_secs(20)), move |event| {
                if matches!(event, whalerein_core::conversation::ViewUpdate::Reset) {
                    watch_resets.fetch_add(1, Ordering::SeqCst);
                }
                if let Some(ready) = ready_tx.take() {
                    let _ = ready.send(());
                }
                true
            })
            .await
    });
    let first_core = core.clone();
    let mut first = tokio::spawn(async move { first_core.wait_session(&first_key, 20000).await });
    let second_core = core.clone();
    let second_session = args[4].clone();
    let mut second = tokio::spawn(async move {
        let key = second_core.session_key(&second_session).await?;
        second_core.wait_session(&key, 10000).await
    });
    let release = PathBuf::from(&args[5]);
    let watch_ready = tokio::time::timeout(Duration::from_secs(8), ready_rx)
        .await
        .is_ok_and(|result| result.is_ok());
    if !watch_ready {
        let code = if watcher.is_finished() {
            match (&mut watcher).await {
                Ok(Err(error)) => error.code(),
                Ok(Ok(_)) => "watch_ended",
                Err(_) => "watch_task_failed",
            }
        } else {
            watcher.abort();
            "watch_not_ready"
        };
        first.abort();
        second.abort();
        println!("{{\"error\":\"{code}\"}}");
        std::process::exit(1);
    }
    if watch_ready {
        std::fs::write(release.with_extension("tail-ready"), b"ready").expect("fixture marker");
    }
    let ready = tokio::time::timeout(Duration::from_secs(8), async {
        while !release.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if ready.is_err()
        || !watch_ready
        || watcher.is_finished()
        || first.is_finished()
        || second.is_finished()
    {
        let first_code = if first.is_finished() {
            match (&mut first).await {
                Ok(Err(error)) => error.code(),
                Ok(Ok(_)) => "finished",
                Err(_) => "task_failed",
            }
        } else {
            first.abort();
            "active"
        };
        let second_code = if second.is_finished() {
            match (&mut second).await {
                Ok(Err(error)) => error.code(),
                Ok(Ok(_)) => "finished",
                Err(_) => "task_failed",
            }
        } else {
            second.abort();
            "active"
        };
        watcher.abort();
        println!("{{\"error\":\"observations_not_overlapping\",\"first\":\"{first_code}\",\"second\":\"{second_code}\"}}");
        std::process::exit(1);
    }
    first.abort();
    let cancelled = (&mut first).await.is_err_and(|error| error.is_cancelled());
    let second_result = second.await;
    let tail_active = !watcher.is_finished();
    let tail_resets = resets.load(Ordering::SeqCst);
    watcher.abort();
    match second_result {
        Ok(Ok(result)) => println!(
            "{{\"first_cancelled\":{cancelled},\"tail_active\":{tail_active},\"tail_resets\":{tail_resets},\"second_reason\":\"{:?}\",\"second_running\":{}}}",
            result.reason, result.state.running
        ),
        _ => {
            println!("{{\"error\":\"second_wait_failed\"}}");
            std::process::exit(1);
        }
    }
}
