use canister_utils::{ClientKey, IcWsCanisterMessage};
use dashmap::{mapref::entry::Entry, DashMap};
use ic_agent::export::Principal;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tracing::{debug, Span};
use metrics::{gauge};

/// State of the WS Gateway that can be shared between threads
#[derive(Clone)]
pub struct GatewayState {
    inner: Arc<GatewayStateInner>,
}

impl GatewayState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(GatewayStateInner::new()),
        }
    }

    /// SAFETY:
    ///
    /// The [Dashmap::entry](https://docs.rs/dashmap/5.5.3/src/dashmap/lib.rs.html#1147-1163) method gets a write lock on the whole shard in which the entry is.
    ///
    /// The lock is moved into either the [OccupiedEntry](https://docs.rs/dashmap/5.5.3/src/dashmap/mapref/entry.rs.html#175-179) or the
    /// [VacantEntry](https://docs.rs/dashmap/5.5.3/src/dashmap/mapref/entry.rs.html#117-120) when they are instantiated and
    /// is released when they go out of scope at the end of this function.
    ///
    /// Therefore, this function is executed atomically.
    ///
    /// Holding a lock accross an '.await' may cause a deadlock.
    /// To prevent deadlocks, this function shall NEVER be async.
    /// This is sufficient to prevent the function from yielding while holding the lock.
    ///
    /// In order to not starve other tasks, make sure to keep the critical section as short as possible.
    pub fn insert_client_channel_and_get_new_poller_state(
        &self,
        canister_id: CanisterPrincipal,
        client_key: ClientKey,
        client_channel_tx: Sender<IcWsCanisterMessage>,
        client_session_span: Span,
    ) -> Option<PollerState> {
        // START OF THE CRITICAL SECTION
        match self.inner.data.entry(canister_id) {
            Entry::Occupied(mut entry) => {
                // the poller has already been started
                // if the poller is active, add client key and sender end of the channel to the poller state
                let poller_state = entry.get_mut();
                poller_state.insert(
                    client_key,
                    ClientSender {
                        sender: client_channel_tx.clone(),
                        span: client_session_span,
                    },
                );

                // Increment the number of clients connected to the canister
                let clients_connected = poller_state.len();
                debug!("Clients connected: {}", clients_connected.to_string());
                gauge!("clients_connected", "canister_id" => canister_id.to_string()).set(clients_connected as f64);
                // the poller shall not be started again
                None
            },
            Entry::Vacant(entry) => {
                // the poller has not been started yet
                // initialize the poller state and add client key and sender end of the channel
                let poller_state = Arc::new(DashMap::with_capacity_and_shard_amount(1024, 1024));
                poller_state.insert(
                    client_key,
                    ClientSender {
                        sender: client_channel_tx.clone(),
                        span: client_session_span,
                    },
                );
                entry.insert(Arc::clone(&poller_state));

                // Increment the number of clients connected to the canister
                let clients_connected = poller_state.len();
                debug!("Clients connected: {}", clients_connected.to_string());
                gauge!("clients_connected", "canister_id" => canister_id.to_string()).set(clients_connected as f64);
                // the poller shall be started
                Some(poller_state)
            },
        }
        // END OF THE CRITICAL SECTION
    }

    /// SAFETY:
    ///
    /// The [Dashmap::entry](https://docs.rs/dashmap/5.5.3/src/dashmap/lib.rs.html#1147-1163) method gets a write lock on the whole shard in which the entry is.
    ///
    /// The lock is moved into the [OccupiedEntry](https://docs.rs/dashmap/5.5.3/src/dashmap/mapref/entry.rs.html#175-179) when it is instantiated and
    /// is released when it goes out of scope at the end of this function.
    ///
    /// Therefore, this function is executed atomically.
    ///
    /// Holding a lock accross an '.await' may cause a deadlock.
    /// To prevent deadlocks, this function shall NEVER be async.
    /// This is sufficient to prevent the function from yielding while holding the lock.
    ///
    /// In order to not starve other tasks, make sure to keep the critical section as short as possible.
    pub fn remove_client(&self, canister_id: CanisterPrincipal, client_key: ClientKey) {
        // START OF THE CRITICAL SECTION
        if let Entry::Occupied(mut entry) = self.inner.data.entry(canister_id) {
            let poller_state = entry.get_mut();
            if poller_state.remove(&client_key).is_none() {
                // as the client was connected, the poller state must contain an entry for 'client_key'
                // if this is encountered it might indicate a race condition
                unreachable!("Client key not found in poller state");
            }

            // Decrement the number of clients connected to the canister
            let clients_connected = poller_state.len();
            debug!("Clients connected: {}", clients_connected.to_string());
            gauge!("clients_connected", "canister_id" => canister_id.to_string()).set(clients_connected as f64);
            // even if this is the last client session for the canister, do not remove the canister from the gateway state
            // this will be done by the poller task
        }
        // END OF THE CRITICAL SECTION

        // this can happen when the poller has failed and the poller state has already been removed
        // indeed, a client session might enter the Close state before the poller side of the channel has been dropped - but after the poller state has been removed -
        // in such a case, the client state as already been removed by the poller, together with the whole poller state
        // therefore there is no need to do anything else here
    }

    pub fn get_active_pollers_count(&self) -> usize {
        self.inner.data.len()
    }

    /// SAFETY:
    ///
    /// The [Dashmap::entry](https://docs.rs/dashmap/5.5.3/src/dashmap/lib.rs.html#1147-1163) method gets a write lock on the whole shard in which the entry is.
    ///
    /// The lock is moved into the [OccupiedEntry](https://docs.rs/dashmap/5.5.3/src/dashmap/mapref/entry.rs.html#175-179) when it is instantiated and
    /// is released when it goes out of scope at the end of this function.
    ///
    /// Therefore, this function is executed atomically.
    ///
    /// Holding a lock accross an '.await' may cause a deadlock.
    /// To prevent deadlocks, this function shall NEVER be async.
    /// This is sufficient to prevent the function from yielding while holding the lock.
    ///
    /// In order to not starve other tasks, make sure to keep the critical section as short as possible.
    pub fn remove_client_if_exists(
        &self,
        canister_id: CanisterPrincipal,
        client_key: ClientKey,
    ) -> ClientRemovalResult {
        // START OF THE CRITICAL SECTION
        if let Entry::Occupied(mut entry) = self.inner.data.entry(canister_id) {
            let poller_state = entry.get_mut();

            // even if this is the last client session for the canister, do not remove the canister from the gateway state
            // this will be done by the poller task
            // returns 'ClientRemovalResult::Removed' if the client was removed, 'ClientRemovalResult::Vacant' if there was no such client
            return {
                match poller_state.remove(&client_key) {
                    Some(_) => {
                        // Decrement the number of clients connected to the canister
                        let clients_connected = poller_state.len();
                        debug!("Clients connected: {}", clients_connected.to_string());
                        gauge!("clients_connected", "canister_id" => canister_id.to_string()).set(clients_connected as f64);

                        ClientRemovalResult::Removed(client_key)
                    },
                    None => ClientRemovalResult::Vacant,
                }
            };
        }
        // END OF THE CRITICAL SECTION

        // this can happen when the poller has failed and the poller state has already been removed
        // indeed, a client session might get an error before the poller side of the channel has been dropped - but after the poller state has been removed -
        // in such a case, the client state has already been removed by the poller, together with the whole poller state
        // therefore there is no need to do anything else here and we pretend that there is no such entry
        ClientRemovalResult::Vacant
    }

    /// SAFETY:
    ///
    /// The [Dashmap::remove_if](https://docs.rs/dashmap/5.5.3/src/dashmap/lib.rs.html#944-969) method gets a write lock on the whole shard in which the entry is.
    ///
    /// The lock is held while checking the condition and removing the entry if the condition is met.
    ///
    /// Therefore, this function is executed atomically.
    ///
    /// This function shall be called only if it is guaranteed that the canister entry exists in the gateway state.
    pub fn remove_canister_if_empty(
        &self,
        canister_id: CanisterPrincipal,
    ) -> CanisterRemovalResult {
        // remove_if returns None if the condition is not met, otherwise it returns the Some(<entry>)
        // if Some, the poller state is empty and therefore the poller shall terminate - return 'CanisterRemovalResult::Empty'
        // if None, the poller state is not empty and therefore there are still clients connected and the poller shall not terminate - return 'CanisterRemovalResult::NotEmpty'
        match self
            .inner
            .data
            .remove_if(&canister_id, |_, poller_state| poller_state.is_empty())
        {
            Some(_) => CanisterRemovalResult::Empty,
            None => CanisterRemovalResult::NotEmpty,
        }
    }

    /// SAFETY:
    ///
    /// The [Dashmap::remove](https://docs.rs/dashmap/5.5.3/src/dashmap/lib.rs.html#930-942) method gets a write lock on the whole shard in which the entry is.
    ///
    /// The lock is held while removing the entry.
    ///
    /// Therefore, this function is executed atomically.
    ///
    /// This function shall be called only if it is guaranteed that the canister entry exists in the gateway state.
    pub fn remove_failed_canister(&self, canister_id: CanisterPrincipal) {
        if let None = self.inner.data.remove(&canister_id) {
            unreachable!("failed canister not found in gateway state");
        }
    }
}

/// State of the WS Gateway consisting of the principal of each canister being polled
/// and the state of each poller
struct GatewayStateInner {
    // the guard returned when locking a dashmap is 'Send', therefore it is critical
    // that it is not held accross .await points
    // more info: https://draft.ryhl.io/blog/shared-mutable-state/
    data: DashMap<CanisterPrincipal, PollerState>,
}

impl GatewayStateInner {
    fn new() -> Self {
        Self {
            data: DashMap::with_capacity_and_shard_amount(32, 32),
        }
    }
}

/// State of each poller consisting of the keys of the clients connected to the poller
/// and the state associated to each client
pub type PollerState = Arc<DashMap<ClientKey, ClientSender>>;

/// Determines whether the client was removed from the poller state or if there was no such client
pub enum ClientRemovalResult {
    /// The client was removed from the poller state
    Removed(ClientKey),
    /// The client was not present in the poller state
    Vacant,
}

/// Determines whether the canister was removed from the gateway state or not (in case there are still clients connected)
pub enum CanisterRemovalResult {
    /// The canister was removed from the gateway state
    Empty,
    /// The canister was not removed from the gateway state
    NotEmpty,
}

/// State of each client consisting of the sender side of the channel used to send canister updates to the client
/// and the span associated to the client session
#[derive(Debug)]
pub struct ClientSender {
    pub sender: Sender<IcWsCanisterMessage>,
    pub span: ClientSessionSpan,
}

pub type ClientSessionSpan = Span;

pub type CanisterPrincipal = Principal;

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::{self, Receiver};

    use super::*;
    use std::{
        thread,
        time::{Duration, Instant},
    };

    #[tokio::test]
    async fn should_insert_new_client_channels_and_get_new_poller_state_once() {
        let clients_count = 1000;
        let gateway_state = GatewayState::new();
        let canister_id = Principal::from_text("aaaaa-aa").unwrap();
        thread::scope(|s| {
            let mut handles = Vec::new();
            for i in 0..clients_count {
                let client_key = ClientKey::new(Principal::anonymous(), i);
                let handle = s.spawn(|| {
                    let (client_channel_tx, _): (
                        Sender<IcWsCanisterMessage>,
                        Receiver<IcWsCanisterMessage>,
                    ) = mpsc::channel(100);

                    gateway_state.insert_client_channel_and_get_new_poller_state(
                        canister_id,
                        client_key,
                        client_channel_tx,
                        Span::current(),
                    )
                });
                handles.push(handle);
            }
            let mut count = 0;
            let mut poller_state: Option<Arc<DashMap<ClientKey, ClientSender>>> = None;
            for h in handles.into_iter() {
                if let Some(state) = h.join().unwrap() {
                    poller_state = Some(state.clone());
                    count += 1;
                }
            }
            assert_eq!(count, 1);
            assert_eq!(
                poller_state.expect("must be some").len(),
                clients_count as usize
            );
        });
    }

    #[tokio::test]
    async fn benchmark_insertions_only() {
        let clients_count = 1000;
        let gateway_state = GatewayState::new();
        let canister_id = Principal::from_text("aaaaa-aa").unwrap();
        thread::scope(|s| {
            let mut handles = Vec::new();
            for i in 0..clients_count {
                let client_key = ClientKey::new(Principal::anonymous(), i);
                let handle = s.spawn(|| {
                    let (client_channel_tx, _): (
                        Sender<IcWsCanisterMessage>,
                        Receiver<IcWsCanisterMessage>,
                    ) = mpsc::channel(100);

                    let start = Instant::now();
                    gateway_state.insert_client_channel_and_get_new_poller_state(
                        canister_id,
                        client_key,
                        client_channel_tx,
                        Span::current(),
                    );
                    Instant::now() - start
                });
                handles.push(handle);
            }
            let mut tot = Duration::from_secs(0);
            for h in handles {
                tot += h.join().unwrap();
            }
            println!(
                "Average for 'insert_client_channel_and_get_new_poller_state' from {} different threads: {:?}",
                clients_count,
                tot / clients_count as u32
            );
        });
    }

    #[tokio::test]
    async fn benchmark_insertions_while_check_if_empty() {
        let iterations = 10_000;
        let gateway_state = GatewayState::new();
        let canister_id = Principal::from_text("aaaaa-aa").unwrap();

        let start = Instant::now();
        let mut tot = Duration::from_secs(0);
        for i in 0..iterations {
            let client_key = ClientKey::new(Principal::anonymous(), i);
            let (client_channel_tx, _): (
                Sender<IcWsCanisterMessage>,
                Receiver<IcWsCanisterMessage>,
            ) = mpsc::channel(100);

            let start = Instant::now();
            gateway_state.insert_client_channel_and_get_new_poller_state(
                canister_id,
                client_key,
                client_channel_tx,
                Span::current(),
            );
            tot += Instant::now() - start;
        }
        let average_idle = tot / iterations as u32;
        let elapsed_idle = Instant::now() - start;

        {
            let gateway_state = gateway_state.clone();
            let canister_id = canister_id.clone();
            thread::spawn(move || loop {
                gateway_state.remove_canister_if_empty(canister_id);
            });
        }

        let start = Instant::now();
        let mut tot = Duration::from_secs(0);
        for i in 0..iterations {
            let client_key = ClientKey::new(Principal::anonymous(), i);
            let (client_channel_tx, _): (
                Sender<IcWsCanisterMessage>,
                Receiver<IcWsCanisterMessage>,
            ) = mpsc::channel(100);

            let start = Instant::now();
            gateway_state.insert_client_channel_and_get_new_poller_state(
                canister_id,
                client_key,
                client_channel_tx,
                Span::current(),
            );
            tot += Instant::now() - start;
        }
        let average_busy = tot / iterations as u32;
        let elapsed_busy = Instant::now() - start;
        println!(
            "Run {} iterations of 'insert_client_channel_and_get_new_poller_state' on the same thread\nElapsed while:
            idle: {:?}
            busy: {:?}
            deterioration: {:?}\nAverage while:
            idle: {:?}
            busy: {:?}
            deterioration: {:?}",
            iterations,
            elapsed_idle,
            elapsed_busy,
            elapsed_busy.as_secs_f64() / elapsed_idle.as_secs_f64(),
            average_idle,
            average_busy,
            average_busy.as_secs_f64() / average_idle.as_secs_f64(),
        );
    }

    #[tokio::test]
    async fn benchmark_check_if_empty_while_insertions() {
        let iterations = 10_000;
        let gateway_state = GatewayState::new();
        let canister_id = Principal::from_text("aaaaa-aa").unwrap();

        let start = Instant::now();
        let mut tot = Duration::from_secs(0);
        for _ in 0..iterations {
            let start = Instant::now();
            gateway_state.remove_canister_if_empty(canister_id);
            tot += Instant::now() - start;
        }
        let average_idle = tot / iterations as u32;
        let elapsed_idle = Instant::now() - start;

        {
            let gateway_state = gateway_state.clone();
            let canister_id = canister_id.clone();
            thread::spawn(move || {
                for i in 0.. {
                    let client_key = ClientKey::new(Principal::anonymous(), i);
                    let (client_channel_tx, _): (
                        Sender<IcWsCanisterMessage>,
                        Receiver<IcWsCanisterMessage>,
                    ) = mpsc::channel(100);

                    gateway_state.insert_client_channel_and_get_new_poller_state(
                        canister_id,
                        client_key,
                        client_channel_tx,
                        Span::current(),
                    );
                }
            });
        }

        let start = Instant::now();
        let mut tot = Duration::from_secs(0);
        for _ in 0..iterations {
            let start = Instant::now();
            gateway_state.remove_canister_if_empty(canister_id);
            tot += Instant::now() - start;
        }
        let average_busy = tot / iterations as u32;
        let elapsed_busy = Instant::now() - start;

        println!(
            "Run {} iterations of 'remove_canister_if_empty' on the same thread\nElapsed while:
            idle: {:?}
            busy: {:?}
            deterioration: {:?}\nAverage while:
            idle: {:?}
            busy: {:?}
            deterioration: {:?}",
            iterations,
            elapsed_idle,
            elapsed_busy,
            elapsed_busy.as_secs_f64() / elapsed_idle.as_secs_f64(),
            average_idle,
            average_busy,
            average_busy.as_secs_f64() / average_idle.as_secs_f64(),
        );
    }

    #[tokio::test]
    async fn simulate_ic_ws() {
        let iterations = 100;
        let gateway_state = GatewayState::new();
        let canister_id = Principal::from_text("aaaaa-aa").unwrap();

        {
            let gateway_state = gateway_state.clone();
            let canister_id = canister_id.clone();
            thread::spawn(move || {
                for i in 0.. {
                    let client_key = ClientKey::new(Principal::anonymous(), i);
                    let (client_channel_tx, _): (
                        Sender<IcWsCanisterMessage>,
                        Receiver<IcWsCanisterMessage>,
                    ) = mpsc::channel(100);

                    gateway_state.insert_client_channel_and_get_new_poller_state(
                        canister_id,
                        client_key,
                        client_channel_tx,
                        Span::current(),
                    );
                    // simulates 100 clients connecting each second
                    thread::sleep(Duration::from_millis(10));
                }
            });
        }

        let mut tot = Duration::from_secs(0);
        for _ in 0..iterations {
            let start = Instant::now();
            gateway_state.remove_canister_if_empty(canister_id);
            tot += Instant::now() - start;
            // simulates a polling iteration of 1 ms
            thread::sleep(Duration::from_millis(1));
        }
        let average = tot / iterations as u32;

        println!(
            "Run {} iterations of 'remove_canister_if_empty' on the same thread while simulating 100 clients connecting each second.
            Average time: {:?}",
            iterations, average,
        );
    }
}
