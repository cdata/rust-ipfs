//! Handles the `/ipfs/bitswap/1.0.0` and `/ipfs/bitswap/1.1.0` protocols. This
//! allows exchanging IPFS blocks.
//!
//! # Usage
//!
//! The `Bitswap` struct implements the `NetworkBehaviour` trait. When used, it
//! will allow providing and reciving IPFS blocks.
use crate::bitswap::ledger::{Ledger, Message, Priority, I, O};
use crate::bitswap::protocol::BitswapConfig;
use crate::bitswap::strategy::{Strategy, StrategyEvent};
use crate::block::{Block, Cid};
use fnv::FnvHashSet;
use futures::prelude::*;
use libp2p::core::swarm::{
    ConnectedPoint, NetworkBehaviour, NetworkBehaviourAction, PollParameters,
};
use libp2p::core::protocols_handler::{OneShotHandler, ProtocolsHandler};
use libp2p::{Multiaddr, PeerId};
use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use tokio::prelude::*;

/// Network behaviour that handles sending and receiving IPFS blocks.
pub struct Bitswap<TSubstream, TStrategy: Strategy> {
    /// Marker to pin the generics.
    marker: PhantomData<TSubstream>,
    /// Queue of events to report to the user.
    events: VecDeque<NetworkBehaviourAction<Message<O>, ()>>,
    /// List of peers to send messages to.
    target_peers: FnvHashSet<PeerId>,
    /// Ledger
    connected_peers: HashMap<PeerId, Ledger>,
    /// Wanted blocks
    wanted_blocks: HashMap<Cid, Priority>,
    /// Strategy
    strategy: TStrategy,
}

impl<TSubstream, TStrategy: Strategy> Bitswap<TSubstream, TStrategy> {
    /// Creates a `Bitswap`.
    pub fn new(strategy: TStrategy) -> Self {
        println!("bitswap: new");
        Bitswap {
            marker: PhantomData,
            events: VecDeque::new(),
            target_peers: FnvHashSet::default(),
            connected_peers: HashMap::new(),
            wanted_blocks: HashMap::new(),
            strategy,
        }
    }

    /// Connect to peer.
    ///
    /// Called from Kademlia behaviour.
    pub fn connect(&mut self, peer_id: PeerId) {
        println!("bitswap: connect");
        if self.wanted_blocks.len() > 0 {
            if self.target_peers.insert(peer_id.clone()) {
                println!("  queuing dial_peer to {}", peer_id.to_base58());
                self.events.push_back(NetworkBehaviourAction::DialPeer { peer_id });
            }
        }
        println!("");
    }

    /// Sends a block to the peer.
    ///
    /// Called from a Strategy.
    pub fn send_block(&mut self, peer_id: PeerId, block: Block) {
        println!("bitswap: send_block");
        let ledger = self.connected_peers.get_mut(&peer_id)
            .expect("Peer not in ledger?!");
        let message = ledger.send_block(block);
        println!("  queuing block for {}", peer_id.to_base58());
        self.events.push_back(NetworkBehaviourAction::SendEvent {
            peer_id,
            event: message,
        });
        println!("");
    }

    /// Queues the wanted block for all peers.
    ///
    /// A user request
    pub fn want_block(&mut self, cid: Cid, priority: Priority) {
        println!("bitswap: want_block");
        for (peer_id, ledger) in self.connected_peers.iter_mut() {
            let message = ledger.want_block(&cid, priority);
            println!("  queuing want for {}", peer_id.to_base58());
            self.events.push_back(NetworkBehaviourAction::SendEvent {
                peer_id: peer_id.to_owned(),
                event: message,
            });
        }
        self.wanted_blocks.insert(cid, priority);
        println!("");
    }

    /// Removes the block from our want list and updates all peers.
    ///
    /// Can be either a user request or be called when the block
    /// was received.
    pub fn cancel_block(&mut self, cid: &Cid) {
        println!("bitswap: cancel_block");
        for (peer_id, ledger) in self.connected_peers.iter_mut() {
            let message = ledger.cancel_block(cid);
            if message.is_some() {
                println!("  queuing cancel for {}", peer_id.to_base58());
                self.events.push_back(NetworkBehaviourAction::SendEvent {
                    peer_id: peer_id.to_owned(),
                    event: message.unwrap(),
                });
            }
        }
        self.wanted_blocks.remove(cid);
        println!("");
    }
}

impl<TSubstream, TStrategy: Strategy> NetworkBehaviour for Bitswap<TSubstream, TStrategy>
where
    TSubstream: AsyncRead + AsyncWrite,
{
    type ProtocolsHandler = OneShotHandler<TSubstream, BitswapConfig, Message<O>, InnerMessage>;
    type OutEvent = ();

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        println!("bitswap: new_handler");
        Default::default()
    }

    fn addresses_of_peer(&mut self, _peer_id: &PeerId) -> Vec<Multiaddr> {
        println!("bitswap: addresses_of_peer");
        Vec::new()
    }

    fn inject_connected(&mut self, peer_id: PeerId, cp: ConnectedPoint) {
        println!("bitswap: inject_connected");
        println!("  peer_id: {}", peer_id.to_base58());
        println!("  connected_point: {:?}", cp);
        let ledger = Ledger::new();
        if !self.wanted_blocks.is_empty() {
            let mut message = Message::new();
            for (cid, priority) in &self.wanted_blocks {
                message.want_block(cid, *priority);
            }
            println!("  queuing wanted blocks");
            self.events.push_back(NetworkBehaviourAction::SendEvent {
                peer_id: peer_id.clone(),
                event: message,
            });
        }
        self.connected_peers.insert(peer_id, ledger);
        println!("");
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId, cp: ConnectedPoint) {
        println!("bitswap: inject_disconnected {:?}", cp);
        println!("  peer_id: {}", peer_id.to_base58());
        println!("  connected_point: {:?}", cp);
        println!("");
        //self.connected_peers.remove(peer_id);
    }

    fn inject_node_event(
        &mut self,
        source: PeerId,
        event: InnerMessage,
    ) {
        println!("bitswap: inject_node_event");
        println!("{:?}", event);
        let message = match event {
            InnerMessage::Rx(message) => {
                message
            },
            InnerMessage::Tx => {
                return;
            },
        };
        println!("  received message");

        // Update the ledger.
        let ledger = self.connected_peers.get_mut(&source)
            .expect("Peer not in ledger?!");
        ledger.update_incoming_stats(&message);

        // Process incoming messages.
        for block in message.blocks() {
            // Cancel the block.
            self.cancel_block(&block.cid());
            self.strategy.process_block(source.clone(), block.to_owned());
        }
        for (cid, priority) in message.want() {
            self.strategy.process_want(source.clone(), cid.to_owned(), *priority);
        }
        // TODO: Remove cancelled `Want` events from the queue.
        // TODO: Remove cancelled blocks from `SendEvent`.
        println!("");
    }

    fn poll(
        &mut self,
        _: &mut PollParameters,
    ) -> Async<NetworkBehaviourAction<
            <Self::ProtocolsHandler as ProtocolsHandler>::InEvent, Self::OutEvent>> {
        println!("bitswap: poll");
        // TODO concat messages to same destination to reduce traffic.
        if let Some(event) = self.events.pop_front() {
            if let NetworkBehaviourAction::SendEvent { peer_id, event } = &event {
                let ledger = self.connected_peers.get_mut(&peer_id)
                    .expect("Peer not in ledger?!");
                ledger.update_outgoing_stats(&event);
                println!("  send_message to {}", peer_id.to_base58());
            }
            println!("{:?}", event);
            println!("");
            return Async::Ready(event);
        }

        match self.strategy.poll() {
            Some(StrategyEvent::Send { peer_id, block }) => {
                self.send_block(peer_id, block);
                task::current().notify();
            }
            None => {}
        }

        println!("");
        Async::NotReady
    }
}

/// Transmission between the `OneShotHandler` and the `BitswapHandler`.
#[derive(Debug)]
pub enum InnerMessage {
    /// We received a `Message` from a remote.
    Rx(Message<I>),
    /// We successfully sent a `Message`.
    Tx,
}

impl From<Message<I>> for InnerMessage {
    #[inline]
    fn from(message: Message<I>) -> InnerMessage {
        InnerMessage::Rx(message)
    }
}

impl From<()> for InnerMessage {
    #[inline]
    fn from(_: ()) -> InnerMessage {
        InnerMessage::Tx
    }
}
