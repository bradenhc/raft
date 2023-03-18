use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use tokio;
use tonic::transport::channel::Channel;

use raft::raft_client::RaftClient;
use raft::raft_server::Raft;
use raft::{AppendEntriesRequest, AppendEntriesResponse, RequestVoteRequest, RequestVoteResponse};

pub mod raft {
    tonic::include_proto!("raft");
}

/// Configuration for this Raft node.
///
pub struct Config {
    pub id: String,
    pub port: u16,
    pub cluster_nodes: Vec<SocketAddr>,
    pub election_timeout_seed: i32,
    pub election_timeout_min: u64,
    pub election_timeout_max: u64,
}

struct Peer {
    id: String,
    client: RaftClient<Channel>,
}

impl Peer {
    async fn connect(id: String, address: String) -> Peer {
        let client = RaftClient::connect(address).await.unwrap();
        Peer { id, client }
    }
}

/// The type or state of the node. In Raft, the node is only ever in one of these states and
/// behaves differently in response to RPC messages depending on the state it is in.
///
enum NodeType {
    Follower,
    Candidate,
    Leader,
}

/// Represents the state of a single entry in the replicated log. Each entry holds a command that
/// manipulates the state machine used for the node's business logic.
///
struct LogEntry {
    /// Flag that indicates whether this entry has been committed to the log.
    committed: bool,

    /// The index of this entry in the log (starting from 1).
    index: u64,

    /// The term during which this entry was applied.
    term: u64,

    /// The command used to manipulate the state machine of the node.
    command: String,
}

/// Represents the state of all the followers the leader node is aware of.
///
#[derive(Default)]
struct FollowerState {
    /// Index of the next log entry to send to this follower.
    next_index: i64,

    /// Index of the highest log entry known to be replicated on this follower.
    match_index: i64,
}

struct Log {
    data: Vec<LogEntry>,
}

impl Log {
    fn new() -> Log {
        Log { data: Vec::new() }
    }

    fn append(&self) {}
}

struct ConsensusState {
    /// The current state the node is in.
    node_type: NodeType,

    /// The latest term this node has seen. This value is persisted.
    term: u64,

    /// Contains the candidate that this node voted for in the current term. Set to `None` if the
    /// node has not voted yet. This value is persisted.
    voted_for: Option<String>,

    /// The log entries owned by the node. The values in the log are persisted.
    log: Log,

    /// Index of the highest log entry known to be committed on this node. The value is volatile and
    /// initializes to 0 on every start of the node, increasing monotonically.
    committed_index: i64,

    /// Index of the highest log entry applied to the state machine. This value is volatile and
    /// initializes to 0 on every start of the node, increasing monotonically.
    last_applied_index: i64,

    /// List of known followers when this node is the leader. The value of this field is `None` if
    /// the node is not the leader. The list of followers is reinitilized after an election.
    followers: Option<HashMap<String, FollowerState>>,
}

impl ConsensusState {
    /// Create a new instance with default initialized values specified in the Raft algorithm.
    ///
    fn new() -> ConsensusState {
        ConsensusState {
            node_type: NodeType::Follower,
            term: 0,
            voted_for: None,
            log: Log::new(),
            committed_index: 0,
            last_applied_index: 0,
            followers: None,
        }
    }
}

pub struct CommandSender<T> {
    chan: tokio::sync::mpsc::Sender<T>,
}

impl<T> CommandSender<T> {
    fn new(chan: tokio::sync::mpsc::Sender<T>) -> CommandSender<T> {
        CommandSender { chan }
    }
}

pub struct CommandReceiver<T> {
    chan: tokio::sync::mpsc::Receiver<T>,
}

impl<T> CommandReceiver<T> {
    fn new(chan: tokio::sync::mpsc::Receiver<T>) -> CommandReceiver<T> {
        CommandReceiver { chan }
    }
}

/// Holds state and logic for a single Raft node.
///
struct Node {
    /// The ID of this node. Guaranteed to be unique in the cluster.
    id: String,

    /// A list of all the peers available in the cluster.
    peers: Vec<Peer>,

    /// State specific to the Raft consesus algorithm for this node.
    state: ConsensusState,

    /// Randomized timeout before leader election is triggered for this node.
    timeout: Duration,

    /// Sender channel used to transmit committed state change commands to the server.
    sender: CommandSender<String>,

    /// Receiver channel used to process commands received by the server.
    receiver: CommandReceiver<String>,

    /// Timer receiver channel that receives a value when the timer expires. This can be used with
    /// the `tokio::select!` macro when waiting for multiple asynchronous conditions to occur.
    timer_rx: tokio::sync::mpsc::Receiver<()>,

    timer_tx: tokio::sync::mpsc::Sender<()>, 
}

pub enum Error {}

/// Starts this node as a member of a cluster as configured in `config`.
///
/// The logic for the Raft consensus algorithm executes in a separate thread. That thread will
/// be spawned in this associated function. The thread will execute asynchronously. When a state
/// change can be applied to the internal state machine, the command will be sent via the
/// provided channel.
///
pub fn spawn(config: Config) -> Result<(CommandSender<String>, CommandReceiver<String>), Error> {
    let (server_tx, server_rx) = tokio::sync::mpsc::channel(16);
    let (raft_tx, raft_rx) = tokio::sync::mpsc::channel(16);

    let mut node = Node {
        id: config.id,
        peers: Vec::new(),
        state: ConsensusState::new(),
        timeout: Duration::from_millis(config.election_timeout_min),
        sender: CommandSender::new(server_tx),
        receiver: CommandReceiver::new(raft_rx),
    };

    // Build the runtime for the new thread.
    //
    // The runtime is created before spawning the thread
    // to more cleanly forward errors if the `unwrap()`
    // panics.
    //
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    std::thread::spawn(move || {
        rt.block_on(async move {});
    });

    Ok((CommandSender::new(raft_tx), CommandReceiver::new(server_rx)))
}

impl Node {
    async fn begin_election(&mut self) {
        // Increment term
        self.state.term += 1;

        // Transition to candidate state
        self.state.node_type = NodeType::Candidate;

        // Vote for self
        self.state.voted_for = Some(self.id.clone());

        // TODO: Send RequestVote message in parallel
        for p in &mut self.peers {}

        // TODO: Set random election timeout

        // Wait until
        //   1) you win
        //     - initialize follower list (next_index, etc.)
        //     - send AppendEntries heartbeat to other nodes
        //   2) another candidate wins
        //     - if receives AppendEntries and message term is >= node term
        //   3) timeout with no winner
        //
    }

    fn vote(&mut self, request: raft::RequestVoteRequest) -> bool {
        if self.state.voted_for.is_none() {
            self.state.voted_for = Some(request.candidate_id);
            return true;
        }
        false
    }

    fn replicate_log(&self) {
        // Send AppendEntries to all followers
        // Wait for majority
        // When replicating, continually send AppendEntries until all follower nodes acknowledge
        //     (even after majority and a response has been sent back to the client)
    }
}

#[tonic::async_trait]
impl Raft for Node {
    async fn request_vote(
        &self,
        request: tonic::Request<RequestVoteRequest>,
    ) -> Result<tonic::Response<RequestVoteResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "RequestVote RPC has not been implemented",
        ))
    }

    async fn append_entries(
        &self,
        request: tonic::Request<AppendEntriesRequest>,
    ) -> Result<tonic::Response<AppendEntriesResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "AppendEntries RPC has not been implemented",
        ))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
