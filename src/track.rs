use std::sync::Arc;

use tokio::sync::Mutex;

use crate::{consts::HEARTBEAT_INTERVAL, KnownServer, State};

pub async fn track_thread(state: Arc<State>, steamid: u64, arc: Arc<Mutex<KnownServer>>) {
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        let mut server = arc.lock().await;

        if server.last_heartbeat.elapsed().unwrap().as_secs() > HEARTBEAT_INTERVAL {
            log::info!("server {} timed out", steamid);
            state.servers.write().await.remove(&steamid);
            return;
        }

        // TODO: use steam to ping the server
    }
}
