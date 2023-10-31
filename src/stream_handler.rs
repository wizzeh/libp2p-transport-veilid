use tokio_crate::time::sleep as async_sleep;
use tokio_crate::time::Duration;

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

use crate::stream::VeilidStreamManager;

pub struct StreamHandler;

impl StreamHandler {
    pub async fn run() {
        loop {
            // get all streams and run maintenance
            let streams = VeilidStreamManager::get_all().await;

            for stream in streams {
                stream
                    // send our status if we haven't sent anything recently
                    .send_status_if_stale()
                    .await
                    // generate messages for whatever is in the buffer to clear it
                    .generate_messages(0)
                    // try send any undelivered messages
                    .send_app_msg()
                    .await;
            }

            VeilidStreamManager::clean().await;
            async_sleep(Duration::from_millis(1000)).await;
        }
    }
}
